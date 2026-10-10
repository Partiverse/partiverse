//! 传输校验(M1-WP06-T03):单文件 job(copyfile 级)完成后 src/dst 条目比对的
//! 哈希降级链 + 两段采样哈希 + 校验和落库。
//!
//! # rc 形状锚定(2026-10-11 本机 rclone 1.75.1 实机实验,run 报告 §DoD①,禁凭记忆)
//! - `operations/list` 默认应答**不含**哈希字段;`{"fs","remote","opt":{"hashTypes":[…]}}`
//!   请求后条目携带 `Hashes:{<type>:<value>}`(local 后端实算,webdav 后端 200 且
//!   静默省略不支持类型;不在全局哈希命名的类型名 → 500 `unknown hash type`)。
//! - 参数 `{fs, remote}` 双键必填(缺 remote → 400 `Didn't find key "remote" in input`,
//!   fs 携带路径不被接受);`remote` 指向文件 → 500 `readdirent … not a directory`;
//!   目录不存在 → 404 `directory not found`。
//! - `Size` 负值 = 「未知」约定(实测 memory 桶条目 `Size:-1`)→ 解析为 None。
//!
//! # 降级链(卡内 ②,顺序即优先级)
//! 1. **共同哈希档**:双端条目存在共同哈希类型(候选集优先序,候外类型排序兜底)
//!    → 比对值;一致 = 通过,不一致 = 内容差异 Fatal。
//! 2. **size 档**:无共同哈希 → 比对 size;不一致 = 传输不完整类 Retryable。
//! 3. **采样档**:size 相等(或缺失)且可采样(仅本地盘 Node,std 读文件)→
//!    双端本地比对两段采样指纹,不一致 = 内容差异 Fatal;单端本地仅作校验和
//!    标注(size 档已通过)。**远端采样读取通道挂 WP07 数据面(卡内显式边界)**。
//! 4. 无任何可比基础(无共同哈希、size 缺失、无可采样端)→ 显式 Incomparable
//!    (零吞错:结果值明示,job 保持 done,校验和列不造值)。
//!
//! 校验失败(Mismatch/校验通道自身 IO 失败/条目缺失/列表失败)→
//! [`crate::jobs::JobManager::mark_verify_failed`] 落 error 终态(Severity 按差异
//! 性质:内容差异/缺条目 = Fatal,尺寸差异 = Retryable,IO 失败 = classify_io)。
//!
//! # 采样指纹(卡内 ③,过渡件)
//! `pv-sample-sha256 = SHA-256("partiverse-sample-v1" ‖ size_be_u64 ‖ 头64KiB ‖ 尾64KiB)`
//! 自写约 30-50 行;原语 sha2 0.11 复用(ADR-0009),V2 整体移交 partisync cas
//! (复用评估 §2,架构 D27)。**已知边界:窗口外(文件中段)篡改不可检**——采样
//! 是变更检测指纹而非完整内容承诺,proptest 性质测试按规格固化该边界。

use std::collections::BTreeMap;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{PartisyError, Severity};
use crate::jobs::{JobErrorKind, JobManager, JobStatus, RcDispatch, io_err};

/// 采样窗口(头/尾各 64KiB;卡内 ③「头N+尾N+size」的 N 定档,≤128KiB 输入)。
pub const SAMPLE_WINDOW_BYTES: u64 = 64 * 1024;
/// 采样指纹方案标注前缀(落库 `checksum` 列格式 `<scheme>:<hex>`)。
pub const SAMPLE_SCHEME: &str = "pv-sample-sha256";
/// 指纹域分隔串(算法/方案版本一经落库即冻结;改算法须换前缀,ADR-0009)。
const SAMPLE_DOMAIN: &[u8] = b"partiverse-sample-v1";

/// 哈希候选集(优先序 = 强度序;全部条目经本机 1.75.1 实测为合法类型名且对
/// local 后端可实算、对 webdav 后端被静默省略,run 报告 §DoD① 实验4/5;
/// 候选外类型经 [`pick_common_hash`] 排序兜底,禁凭记忆增删)。
pub const HASH_CANDIDATES: &[&str] = &["sha256", "sha1", "md5", "crc32"];

/// rc 载荷异常(Fatal;jobs 同款形状,附原始载荷截断)。
fn invalid(where_: &str, problem: &str, reply: &Value) -> PartisyError {
    let brief: String = reply.to_string().chars().take(200).collect();
    JobErrorKind::Invalid(format!("{where_}: {problem}: {brief}")).fatal()
}

/// fs 串 → 本地盘路径(保守识别:仅 `/` 开头的 unix 绝对路径;带 `<name>:`
/// 前缀的 remote、Windows 盘符/相对路径一律 None → 链路降级 size 档不采样,
/// 误判方向 = 少采样不误校验,Windows 通道本就不可用,见 fsops 模块声明)。
#[must_use]
pub fn local_fs_path(fs: &str) -> Option<&Path> {
    match fs.strip_prefix('/') {
        Some(_) => Some(Path::new(fs)),
        None => None,
    }
}

/// 校验目标文件全路径(fs 基座 + 父目录 + 文件名;仅本地盘基座可解析)。
fn local_file_path(fs_base: &str, parent: &str, name: &str) -> Option<PathBuf> {
    local_fs_path(fs_base).map(|base| {
        let mut path = base.to_path_buf();
        if !parent.is_empty() {
            path.push(parent);
        }
        path.push(name);
        path
    })
}

/// 两段采样指纹(卡内 ③):SHA-256(域串 ‖ size 大端 ‖ 头 ‖ 尾)。
/// 同 (head, tail, size) 恒同指纹;size 参与混淆防「同窗异长」碰撞。
#[must_use]
pub fn sample_fingerprint(head: &[u8], tail: &[u8], size: u64) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(SAMPLE_DOMAIN);
    hasher.update(size.to_be_bytes());
    hasher.update(head);
    hasher.update(tail);
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{SAMPLE_SCHEME}:{hex}")
}

/// 本地文件采样(std 读文件,卡内 ② 边界:仅限本地盘 Node):返回
/// (实读文件长, 采样指纹)。长度 ≤ 2 窗口 = 整文件入头段、尾段置空(确定性);
/// 否则读头 64KiB + 尾 64KiB。IO 失败 severity 经核心 `classify_io` 表驱动
/// (瞬态读失败 Retryable,零吞错)。
///
/// # Errors
/// open/metadata/read 失败 → severity 保真上浮(原始错误挂根因链)。
pub fn sample_local_file(path: &Path) -> Result<(u64, String), PartisyError> {
    let mut file = std::fs::File::open(path).map_err(io_err)?;
    let len = file.metadata().map_err(io_err)?.len();
    let window = SAMPLE_WINDOW_BYTES;
    let (head, tail) = if len <= window * 2 {
        // 整读分支:len ≤ 128KiB,容量上界确定(32 位平台亦安全)。
        let mut whole = Vec::with_capacity(len as usize);
        file.read_to_end(&mut whole).map_err(io_err)?;
        (whole, Vec::new())
    } else {
        let mut head = vec![0u8; window as usize];
        file.read_exact(&mut head).map_err(io_err)?;
        file.seek(SeekFrom::Start(len - window)).map_err(io_err)?;
        let mut tail = vec![0u8; window as usize];
        file.read_exact(&mut tail).map_err(io_err)?;
        (head, tail)
    };
    Ok((len, sample_fingerprint(&head, &tail, len)))
}

/// operations/list 条目解析结果(只存路径与元数据,红线 3)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// 条目名(实测键 `Name`;匹配目标文件名用)。
    pub name: String,
    /// 字节数;`None` = 后端未披露(缺键/null/负值,实测 `Size:-1` = 未知约定)。
    pub size: Option<u64>,
    /// 哈希表(实测键 `Hashes`;空串值 = 该类型无哈希,解析时剔除)。
    pub hashes: BTreeMap<String, String>,
}

/// 解析单条 operations/list 条目(实测形状,模块头锚定):目录条目 → None
/// (调用方按名匹配);字段损坏(类型错)→ Fatal,零猜测。
///
/// # Errors
/// 条目非对象 / `IsDir` 缺失或非 bool / `Name` 非 str / `Size` 非整数 /
/// `Hashes` 非对象或值非字符串 → Fatal。
pub fn parse_list_item(item: &Value) -> Result<Option<FileEntry>, PartisyError> {
    let Some(obj) = item.as_object() else {
        return Err(invalid("operations/list", "entry is not an object", item));
    };
    let is_dir = obj
        .get("IsDir")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid("operations/list", "missing bool `IsDir`", item))?;
    if is_dir {
        return Ok(None);
    }
    let name = obj
        .get("Name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("operations/list", "missing string `Name`", item))?
        .to_owned();
    let size = match obj.get("Size") {
        None | Some(Value::Null) => None,
        Some(raw) => {
            let signed = raw
                .as_i64()
                .ok_or_else(|| invalid("operations/list", "`Size` is not an integer", item))?;
            // 负值 = 后端「未知」约定(实测 -1);u64 溢出按损坏拒读。
            if signed < 0 {
                None
            } else {
                Some(
                    u64::try_from(signed)
                        .map_err(|_| invalid("operations/list", "`Size` overflows u64", item))?,
                )
            }
        }
    };
    let mut hashes = BTreeMap::new();
    match obj.get("Hashes") {
        None | Some(Value::Null) => {}
        Some(raw) => {
            let Some(map) = raw.as_object() else {
                return Err(invalid(
                    "operations/list",
                    "`Hashes` is not an object",
                    item,
                ));
            };
            for (kind, value) in map {
                let text = value.as_str().ok_or_else(|| {
                    invalid("operations/list", "`Hashes` value is not a string", item)
                })?;
                if !text.is_empty() {
                    hashes.insert(kind.clone(), text.to_owned());
                }
            }
        }
    }
    Ok(Some(FileEntry { name, size, hashes }))
}

/// 共同哈希择取(降级链第 1 档输入):先按 [`HASH_CANDIDATES`] 优先序,
/// 候选外共同类型按字典序兜底(确定性;择优不漏配)。返回 (类型, src 值, dst 值)。
#[must_use]
pub fn pick_common_hash<'a>(
    src: &'a FileEntry,
    dst: &'a FileEntry,
) -> Option<(&'a str, &'a str, &'a str)> {
    for kind in HASH_CANDIDATES {
        if let (Some(sv), Some(dv)) = (src.hashes.get(*kind), dst.hashes.get(*kind)) {
            return Some((kind, sv, dv));
        }
    }
    src.hashes
        .iter()
        .filter_map(|(kind, sv)| {
            dst.hashes
                .get(kind)
                .map(|dv| (kind.as_str(), sv.as_str(), dv.as_str()))
        })
        .min_by_key(|(kind, _, _)| *kind)
}

/// 校验档位(降级链结论;卡内 ② 链序)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyTier {
    /// 共同哈希一致(最强)。
    Hash,
    /// 双端本地采样指纹一致。
    Sampled,
    /// 仅 size 一致(最弱;可能附单端本地采样标注)。
    SizeOnly,
}

/// 条目对裁决(纯链核;IO 仅本地采样读,severity 保真上浮)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairVerdict {
    /// 通过。
    Match {
        tier: VerifyTier,
        /// 落库校验和标注(hash 档 = `<type>:<value>`;sampled/size 档 = 采样
        /// 指纹或 None)。
        checksum: Option<String>,
    },
    /// 校验失败(差异性质定 Severity:内容差异/条目缺失 = Fatal,尺寸差异 = Retryable)。
    Mismatch { severity: Severity, detail: String },
    /// 显式不可比(无任何可比基础;零吞错,job 保持 done)。
    Incomparable {
        reason: String,
        checksum: Option<String>,
    },
}

/// 降级链核(卡内 ②,表驱动可测):顺序 = 共同哈希 → size → 采样。
/// `src_local`/`dst_local` = 本地盘侧的文件全路径(None = 非本地/不可采样)。
/// 本地采样 IO 失败 → Err(severity 按 classify_io)。
///
/// # Errors
/// 本地采样读失败(severity 保真)。
pub fn verify_pair(
    src: &FileEntry,
    dst: &FileEntry,
    src_local: Option<&Path>,
    dst_local: Option<&Path>,
) -> Result<PairVerdict, PartisyError> {
    // 第 1 档:共同哈希。
    if let Some((kind, sv, dv)) = pick_common_hash(src, dst) {
        return Ok(if sv == dv {
            PairVerdict::Match {
                tier: VerifyTier::Hash,
                checksum: Some(format!("{kind}:{sv}")),
            }
        } else {
            PairVerdict::Mismatch {
                severity: Severity::Fatal,
                detail: format!("checksum mismatch ({kind}): src={sv} dst={dv}"),
            }
        });
    }
    // 第 2 档:size。
    match (src.size, dst.size) {
        (Some(s), Some(d)) if s != d => Ok(PairVerdict::Mismatch {
            severity: Severity::Retryable,
            detail: format!("size mismatch: src={s} dst={d}"),
        }),
        (Some(_), Some(_)) => {
            // 第 3 档:size 相等才采样。双端本地 = 比对;单端本地 = 标注。
            sample_tier(src_local, dst_local)
        }
        // size 缺失:双端本地仍可采样裁决;否则显式不可比(附可得标注)。
        _ => match (src_local, dst_local) {
            (Some(_), Some(_)) => sample_tier(src_local, dst_local),
            _ => {
                let checksum = sample_annotation(src_local.or(dst_local))?;
                Ok(PairVerdict::Incomparable {
                    reason: "entries expose neither common hash nor size; \
                             no samplable local side"
                        .to_owned(),
                    checksum,
                })
            }
        },
    }
}

/// 采样档:双端本地比对指纹(不一致 = 内容差异 Fatal);单端本地 size 档已过,
/// 采样仅作校验和标注。
fn sample_tier(
    src_local: Option<&Path>,
    dst_local: Option<&Path>,
) -> Result<PairVerdict, PartisyError> {
    match (src_local, dst_local) {
        (Some(sp), Some(dp)) => {
            let (_, sfp) = sample_local_file(sp)?;
            let (_, dfp) = sample_local_file(dp)?;
            Ok(if sfp == dfp {
                PairVerdict::Match {
                    tier: VerifyTier::Sampled,
                    checksum: Some(sfp),
                }
            } else {
                PairVerdict::Mismatch {
                    severity: Severity::Fatal,
                    detail: format!("sampled checksum mismatch: src={sfp} dst={dfp}"),
                }
            })
        }
        (sp, dp) => Ok(PairVerdict::Match {
            tier: VerifyTier::SizeOnly,
            checksum: sample_annotation(sp.or(dp))?,
        }),
    }
}

/// 单端本地采样标注(None 端 → None;IO 失败 severity 保真上浮,零吞错)。
fn sample_annotation(path: Option<&Path>) -> Result<Option<String>, PartisyError> {
    match path {
        None => Ok(None),
        Some(path) => Ok(Some(sample_local_file(path)?.1)),
    }
}

/// 校验目标(单文件 job 的 src/dst 定位;fs 基座 + 父目录 + 文件名)。
#[derive(Debug, Clone, Copy)]
pub struct VerifyTargets<'a> {
    /// 源 fs 基座(`"remote:"` 或本地裸路径)。
    pub src_fs_base: &'a str,
    /// 源父目录(根 = "")。
    pub src_parent: &'a str,
    /// 源文件名。
    pub src_name: &'a str,
    /// 目标 fs 基座。
    pub dst_fs_base: &'a str,
    /// 目标父目录。
    pub dst_parent: &'a str,
    /// 目标文件名。
    pub dst_name: &'a str,
}

/// verify_job 结论(编排面;失败已落 error 终态)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// 通过(checksum 已落 `checksum` 列,若有)。
    Verified {
        tier: VerifyTier,
        checksum: Option<String>,
    },
    /// 显式不可比(job 保持 done;采样标注可落列)。
    Incomparable {
        reason: String,
        checksum: Option<String>,
    },
    /// 校验失败(已落 error 终态,Severity 按差异性质;卡内 ④)。
    Failed { severity: Severity, detail: String },
}

/// 拉取单条目(卡内 ② 数据面):`operations/list` 于父目录
/// (参数形状 = 模块头实测锚定:`{fs, remote, opt:{hashTypes}}`),按名匹配
/// 文件条目;未命中 → None(目录级条目集合比对属目录 job 范畴,本卡边界外)。
///
/// # Errors
/// rc 失败(severity 保真)/载荷损坏(Fatal)上浮。
fn fetch_entry(
    dispatch: &impl RcDispatch,
    fs_base: &str,
    parent: &str,
    name: &str,
) -> Result<Option<FileEntry>, PartisyError> {
    let reply = dispatch.call(
        "operations/list",
        &serde_json::json!({
            "fs": fs_base,
            "remote": parent,
            "opt": { "hashTypes": HASH_CANDIDATES },
        }),
    )?;
    let list = reply
        .get("list")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("operations/list", "missing array `list`", &reply))?;
    for item in list {
        if let Some(entry) = parse_list_item(item)?
            && entry.name == name
        {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

/// 单侧条目消解:拉取成功 → [`SideResult::Entry`];缺条目/列表失败 → 落
/// error 终态(severity 差异性质/保真)并携带 [`SideResult::Failed`] 结论。
enum SideResult {
    Entry(FileEntry),
    Failed(VerifyOutcome),
}

/// 见 [`SideResult`]。
fn resolve_side(
    manager: &JobManager,
    dispatch: &impl RcDispatch,
    id: &str,
    side: &str,
    fs_base: &str,
    parent: &str,
    name: &str,
) -> Result<SideResult, PartisyError> {
    match fetch_entry(dispatch, fs_base, parent, name) {
        Ok(Some(entry)) => Ok(SideResult::Entry(entry)),
        Ok(None) => {
            let detail =
                format!("entry not found after transfer ({side}): {name} in {fs_base}{parent}");
            manager.mark_verify_failed(id, &detail, Severity::Fatal)?;
            Ok(SideResult::Failed(VerifyOutcome::Failed {
                severity: Severity::Fatal,
                detail,
            }))
        }
        Err(err) => {
            let detail = format!("verification listing failed ({side}): {err}");
            manager.mark_verify_failed(id, &detail, err.severity)?;
            Ok(SideResult::Failed(VerifyOutcome::Failed {
                severity: err.severity,
                detail,
            }))
        }
    }
}

/// 单文件 job 校验编排(卡内 ②④:job 完成后 src/dst 条目比对;失败落 error
/// 终态):行须 done(校验发生于完成后;非 done = Fatal)→ 双端拉条目 →
/// [`verify_pair`] 降级链 → Match/Incomparable 落 `checksum` 列;
/// Mismatch/条目缺失/列表失败/采样 IO 失败 → [`JobManager::mark_verify_failed`]
/// (severity 差异性质/保真)并返回 [`VerifyOutcome::Failed`]。
///
/// 校验范围 = 单文件 job(copyfile 级;卡内边界);目录级 = 条目集合比对,
/// 非本卡范畴。远端采样不可用(挂 WP07):链路自动降级,零静默(档位/原因
/// 均在结论值中)。
///
/// # Errors
/// 行不存在/非 done(Fatal)、落库失败(DB;含落终态/写列本身失败)上浮;
/// 采样 IO 失败**不外浮**——与 Mismatch 同路经 [`JobManager::mark_verify_failed`]
/// 落 error 终态(severity = classify_io 保真)后以 [`VerifyOutcome::Failed`]
/// 返回(对齐模块头契约与卡内 ④)。
pub fn verify_job(
    manager: &JobManager,
    dispatch: &impl RcDispatch,
    id: &str,
    targets: &VerifyTargets<'_>,
) -> Result<VerifyOutcome, PartisyError> {
    let record = manager.get(id)?;
    if record.status != JobStatus::Done {
        return Err(JobErrorKind::Invalid(format!(
            "verification requires a done job: `{id}` is `{}`",
            record.status.as_str()
        ))
        .fatal());
    }
    let src_local = local_file_path(targets.src_fs_base, targets.src_parent, targets.src_name);
    let dst_local = local_file_path(targets.dst_fs_base, targets.dst_parent, targets.dst_name);
    let src = match resolve_side(
        manager,
        dispatch,
        id,
        "src",
        targets.src_fs_base,
        targets.src_parent,
        targets.src_name,
    )? {
        SideResult::Entry(entry) => entry,
        SideResult::Failed(outcome) => return Ok(outcome),
    };
    let dst = match resolve_side(
        manager,
        dispatch,
        id,
        "dst",
        targets.dst_fs_base,
        targets.dst_parent,
        targets.dst_name,
    )? {
        SideResult::Entry(entry) => entry,
        SideResult::Failed(outcome) => return Ok(outcome),
    };
    // 采样读 IO 失败 = 校验通道自身失败(模块头契约、卡内 ④):与 Mismatch 同路
    // 落 error 终态(severity = classify_io 保真),不留 done、不外浮——调用方
    // 恒得 VerifyOutcome;仅落库本身失败才上浮。
    match verify_pair(&src, &dst, src_local.as_deref(), dst_local.as_deref()) {
        Ok(PairVerdict::Match { tier, checksum }) => {
            if let Some(text) = &checksum {
                manager.set_checksum(id, text)?;
            }
            Ok(VerifyOutcome::Verified { tier, checksum })
        }
        Ok(PairVerdict::Incomparable { reason, checksum }) => {
            if let Some(text) = &checksum {
                manager.set_checksum(id, text)?;
            }
            Ok(VerifyOutcome::Incomparable { reason, checksum })
        }
        Ok(PairVerdict::Mismatch { severity, detail }) => {
            manager.mark_verify_failed(id, &detail, severity)?;
            Ok(VerifyOutcome::Failed { severity, detail })
        }
        Err(err) => {
            let severity = err.severity;
            let detail = format!("verification sampling io failed: {err}");
            manager.mark_verify_failed(id, &detail, severity)?;
            Ok(VerifyOutcome::Failed { severity, detail })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::PathBuf;

    use proptest::prelude::*;
    use serde_json::json;

    use super::*;
    use crate::error::Severity as Sev;
    use crate::jobs::kind_of;

    /// Fake rc 分发(仅测试,Fake* 纪律):脚本化依序应答并记录调用。
    struct FakeScriptDispatch {
        replies: RefCell<VecDeque<Value>>,
        calls: RefCell<Vec<(String, Value)>>,
    }

    impl FakeScriptDispatch {
        fn scripted(replies: &[Value]) -> Self {
            Self {
                replies: RefCell::new(replies.iter().cloned().collect()),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.borrow().len()
        }
    }

    impl RcDispatch for FakeScriptDispatch {
        fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError> {
            self.calls
                .borrow_mut()
                .push((method.to_owned(), params.clone()));
            self.replies
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| JobErrorKind::Invalid("script exhausted".into()).fatal())
        }
    }

    /// 实测条目形状(2026-10-11,rclone 1.75.1,opt.hashTypes=["sha256","sha1",
    /// "md5","crc32"] 请求 local 后端;run 报告 §DoD① 实验2/3,禁凭记忆)。
    fn anchored_item(name: &str, size: i64, hashes: Value) -> Value {
        json!({
            "Path": name, "Name": name, "Size": size,
            "MimeType": "application/octet-stream",
            "ModTime": "2026-10-11T00:00:00.000000000+08:00", "IsDir": false,
            "Hashes": hashes,
        })
    }

    /// 唯一临时文件路径(测试造数;调用方负责删除)。
    fn temp_file(tag: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pv-checksum-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, bytes).expect("temp file write");
        path
    }

    /// 内存条目构造捷径(size = None 传 -1;hashes 传 json 对象)。
    fn entry(name: &str, size: i64, hashes: Value) -> FileEntry {
        parse_list_item(&anchored_item(name, size, hashes))
            .expect("fixture parses")
            .expect("file entry")
    }

    #[test]
    fn parse_list_item_matches_anchored_shapes() {
        // 模块头实测锚定:默认形状(无 Hashes)/带哈希/Size=-1/目录条目。
        let with_hash = parse_list_item(&anchored_item(
            "f.bin",
            24,
            json!({"md5": "aaa", "sha1": "bbb", "crc32": ""}),
        ))
        .expect("parses")
        .expect("file");
        assert_eq!(with_hash.name, "f.bin");
        assert_eq!(with_hash.size, Some(24));
        assert_eq!(with_hash.hashes.get("md5").map(String::as_str), Some("aaa"));
        // 空串哈希值剔除(该类型无哈希)。
        assert!(!with_hash.hashes.contains_key("crc32"));
        // 无哈希请求/后端省略(webdav 实测形态)→ 空表。
        let bare = json!({"Path": "f", "Name": "f", "Size": 24,
            "MimeType": "x", "ModTime": "t", "IsDir": false});
        let entry = parse_list_item(&bare).expect("parses").expect("file");
        assert!(entry.hashes.is_empty());
        // Size 负值 = 未知约定(实测 memory 桶 -1)→ None;缺键同样 → None。
        assert_eq!(entry_with_size(-1).size, None);
        assert_eq!(
            parse_list_item(&json!({"Name": "f", "IsDir": false}))
                .expect("parses")
                .expect("file")
                .size,
            None
        );
        // 目录条目 → None;字段损坏 → Fatal。
        let dir = json!({"Path": "d", "Name": "d", "Size": -1,
            "MimeType": "inode/directory", "ModTime": "t", "IsDir": true});
        assert_eq!(parse_list_item(&dir).expect("parses"), None);
        for bad in [
            json!({"Name": "f", "Size": "24", "IsDir": false}), // Size 字符串
            json!({"Name": "f", "Size": 1.5, "IsDir": false}),  // Size 浮点
            json!({"Size": 1, "IsDir": false}),                 // 缺 Name
            json!({"Name": "f", "Size": 1}),                    // 缺 IsDir
            json!({"Name": "f", "Size": 1, "IsDir": false, "Hashes": {"md5": 1}}), // 值非串
            json!([]),
        ] {
            let err = parse_list_item(&bad).expect_err("corruption surfaces");
            assert_eq!(err.severity, Sev::Fatal, "{bad}");
        }
    }

    /// 单字段条目(size 档损坏样例用)。
    fn entry_with_size(size: i64) -> FileEntry {
        parse_list_item(&json!({"Name": "f", "Size": size, "IsDir": false}))
            .expect("parses")
            .expect("file")
    }

    #[test]
    fn pick_common_hash_prefers_candidates_then_sorted_fallback() {
        // 候选优先序:sha256 > sha1 > md5 > crc32(即使 md5 也相等,sha1 先中);
        // 候选外共同类型(whirlpool)字典序兜底;无共同 → None。
        let a = entry("f", 1, json!({"sha1": "s1", "md5": "m"}));
        let b = entry("f", 1, json!({"sha1": "S1", "md5": "m", "crc32": "c"}));
        assert_eq!(
            pick_common_hash(&a, &b).map(|(k, _, _)| k),
            Some("sha1"),
            "候选优先序先中 sha1"
        );
        let wa = entry("f", 1, json!({"whirlpool": "w1", "zoo": "z"}));
        let wb = entry("f", 1, json!({"whirlpool": "w2", "zoo": "z2"}));
        assert_eq!(
            pick_common_hash(&wa, &wb).map(|(k, _, _)| k),
            Some("whirlpool"),
            "候选外字典序兜底"
        );
        let lonely = entry("f", 1, json!({"md5": "m"}));
        let other = entry("f", 1, json!({"sha1": "s"}));
        assert_eq!(pick_common_hash(&lonely, &other), None);
    }

    #[test]
    fn degradation_chain_table_driven() {
        // 卡内 ⑤ 表驱动:链序 = 共同哈希 → size → 采样;差异性质定 Severity;
        // 显式不可比(零吞错)。本地侧用真实临时文件(std 读)。
        let body = b"partiverse degradation chain fixture body".repeat(4);
        let same_a = temp_file("chain-a", &body);
        let same_b = temp_file("chain-b", &body);
        let mut differs_inside = body.clone();
        differs_inside[7] ^= 0x20; // 头 64KiB 窗口内翻转
        let diff_b = temp_file("chain-c", &differs_inside);
        let hs = |v: &str| json!({ "md5": v }); // 源侧独有类型(不构成共同哈希)
        let hd = |v: &str| json!({ "sha1": v }); // 目标侧独有类型

        struct Row {
            tag: &'static str,
            src: FileEntry,
            dst: FileEntry,
            src_local: Option<PathBuf>,
            dst_local: Option<PathBuf>,
            want: PairVerdict,
        }
        let rows = vec![
            // ① 共同哈希一致 → Hash 档,标注 = type:value。
            Row {
                tag: "hash-equal",
                src: entry("f", 24, json!({"sha256": "AAA", "md5": "m"})),
                dst: entry("f", 24, json!({"sha256": "AAA"})),
                src_local: None,
                dst_local: None,
                want: PairVerdict::Match {
                    tier: VerifyTier::Hash,
                    checksum: Some("sha256:AAA".into()),
                },
            },
            // ② 共同哈希差异 → 内容差异 Fatal(即使其他类型相等,优先序先中即裁)。
            Row {
                tag: "hash-differ",
                src: entry("f", 24, json!({"sha1": "s1", "md5": "same"})),
                dst: entry("f", 24, json!({"sha1": "s2", "md5": "same"})),
                src_local: None,
                dst_local: None,
                want: PairVerdict::Mismatch {
                    severity: Sev::Fatal,
                    detail: "checksum mismatch (sha1): src=s1 dst=s2".into(),
                },
            },
            // ③ 无共同哈希(src 仅 md5 / dst 仅 sha1)+ size 差异 → 不完整 Retryable。
            Row {
                tag: "size-differ",
                src: entry("f", 100, json!({"md5": "only-src"})),
                dst: entry("f", 90, json!({"sha1": "only-dst"})),
                src_local: None,
                dst_local: None,
                want: PairVerdict::Mismatch {
                    severity: Sev::Retryable,
                    detail: "size mismatch: src=100 dst=90".into(),
                },
            },
            // ④ 无共同哈希 + size 相等 + 双端本地(内容同)→ Sampled 档。
            Row {
                tag: "sample-equal",
                src: entry("f", body.len() as i64, hs("only-src")),
                dst: entry("f", body.len() as i64, hd("only-dst")),
                src_local: Some(same_a.clone()),
                dst_local: Some(same_b.clone()),
                want: PairVerdict::Match {
                    tier: VerifyTier::Sampled,
                    checksum: Some(sample_local_file(&same_a).expect("sample").1),
                },
            },
            // ⑤ 双端本地(窗口内差异)→ 采样内容差异 Fatal。
            Row {
                tag: "sample-differ",
                src: entry("f", body.len() as i64, hs("only-src")),
                dst: entry("f", body.len() as i64, hd("only-dst")),
                src_local: Some(same_a.clone()),
                dst_local: Some(diff_b.clone()),
                want: PairVerdict::Mismatch {
                    severity: Sev::Fatal,
                    detail: format!(
                        "sampled checksum mismatch: src={} dst={}",
                        sample_local_file(&same_a).expect("sample").1,
                        sample_local_file(&diff_b).expect("sample").1
                    ),
                },
            },
            // ⑥ 单端本地 → SizeOnly 档 + 本地采样标注(远端采样挂 WP07 的降级面)。
            Row {
                tag: "one-local",
                src: entry("f", 7, hs("only-src")),
                dst: entry("f", 7, hd("only-dst")),
                src_local: Some(same_a.clone()),
                dst_local: None,
                want: PairVerdict::Match {
                    tier: VerifyTier::SizeOnly,
                    checksum: Some(sample_local_file(&same_a).expect("sample").1),
                },
            },
            // ⑦ 零本地 → SizeOnly 档无标注。
            Row {
                tag: "no-local",
                src: entry("f", 7, hs("only-src")),
                dst: entry("f", 7, hd("only-dst")),
                src_local: None,
                dst_local: None,
                want: PairVerdict::Match {
                    tier: VerifyTier::SizeOnly,
                    checksum: None,
                },
            },
            // ⑧ size 缺失 + 双端本地 → 采样裁决(同内容 Sampled)。
            Row {
                tag: "no-size-both-local",
                src: entry("f", -1, hs("only-src")),
                dst: entry("f", -1, hd("only-dst")),
                src_local: Some(same_a.clone()),
                dst_local: Some(same_b.clone()),
                want: PairVerdict::Match {
                    tier: VerifyTier::Sampled,
                    checksum: Some(sample_local_file(&same_a).expect("sample").1),
                },
            },
            // ⑨ size 缺失 + 零本地 → 显式不可比(零吞错)。
            Row {
                tag: "incomparable",
                src: entry("f", -1, hs("only-src")),
                dst: entry("f", -1, hd("only-dst")),
                src_local: None,
                dst_local: None,
                want: PairVerdict::Incomparable {
                    reason: "entries expose neither common hash nor size; \
                             no samplable local side"
                        .into(),
                    checksum: None,
                },
            },
        ];
        for row in &rows {
            let got = verify_pair(
                &row.src,
                &row.dst,
                row.src_local.as_deref(),
                row.dst_local.as_deref(),
            )
            .unwrap_or_else(|_| panic!("case {}: unexpected err", row.tag));
            assert_eq!(got, row.want, "case {}", row.tag);
        }
        // 采样 IO 失败(本地路径不存在)→ severity 保真上浮(NotFound = Retryable)。
        let ghost = std::env::temp_dir().join("pv-checksum-does-not-exist-9");
        let err = verify_pair(
            &entry("f", -1, hs("only-src")),
            &entry("f", -1, hd("only-dst")),
            Some(&ghost),
            Some(&ghost),
        )
        .expect_err("io surfaces");
        assert_eq!(err.severity, Sev::Retryable);
        for path in [&same_a, &same_b, &diff_b] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn local_fs_path_is_conservative() {
        // 仅 `/` 前缀 = 本地;remote 前缀/裸相对 → None(降级不误采样)。
        assert_eq!(
            local_fs_path("/tmp/x").map(|p| p.as_os_str()),
            Some(std::ffi::OsStr::new("/tmp/x"))
        );
        assert_eq!(local_fs_path("pvsrc:"), None);
        assert_eq!(local_fs_path("pvsrc:sub/f"), None);
        assert_eq!(local_fs_path(""), None);
        assert_eq!(local_fs_path("C:\\data"), None);
        assert_eq!(local_fs_path("relative/path"), None);
    }

    #[test]
    fn fingerprint_binds_domain_size_and_is_hex_annotated() {
        // 空输入有确定值;标注格式 = <scheme>:<64hex>;域串+size 参与混淆。
        let fp = sample_fingerprint(&[], &[], 0);
        assert!(fp.starts_with("pv-sample-sha256:"));
        assert_eq!(fp.len(), "pv-sample-sha256:".len() + 64);
        assert_ne!(fp, sample_fingerprint(&[], &[], 1));
        assert_eq!(
            sample_fingerprint(b"a", b"b", 2),
            sample_fingerprint(b"a", b"b", 2),
            "确定性:同入同出"
        );
        assert_ne!(
            sample_fingerprint(&[1, 2, 3], &[4], 5),
            sample_fingerprint(&[1, 2, 3], &[4, 0], 5),
            "尾段长度参与混淆"
        );
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 48, ..proptest::test_runner::Config::default()
        })]

        /// 卡内 ⑤ 性质:确定性(同入同出)。
        #[test]
        fn prop_fingerprint_deterministic(
            head in prop::collection::vec(any::<u8>(), 0..=300),
            tail in prop::collection::vec(any::<u8>(), 0..=300),
            size in any::<u64>(),
        ) {
            prop_assert_eq!(sample_fingerprint(&head, &tail, size), sample_fingerprint(&head, &tail, size));
        }

        /// 卡内 ⑤ 性质:窗口内单字节翻转必改指纹(SHA-256 雪崩;等值概率 2^-256,
        /// 实践零抖动)。
        #[test]
        fn prop_fingerprint_sensitive_within_window(
            head in prop::collection::vec(any::<u8>(), 1..=300),
            tail in prop::collection::vec(any::<u8>(), 0..=300),
            size in any::<u64>(),
            idx in 0usize..300,
        ) {
            let idx = idx % head.len();
            let mut flipped = head.clone();
            flipped[idx] ^= 0x20;
            prop_assert_ne!(sample_fingerprint(&head, &tail, size), sample_fingerprint(&flipped, &tail, size));
        }

        /// 卡内 ⑤ 性质:size 参与混淆(同窗异长必异指纹)。
        #[test]
        fn prop_fingerprint_binds_size(
            head in prop::collection::vec(any::<u8>(), 0..=64),
            tail in prop::collection::vec(any::<u8>(), 0..=64),
            size in any::<u64>(),
        ) {
            prop_assert_ne!(sample_fingerprint(&head, &tail, size), sample_fingerprint(&head, &tail, size ^ 1));
        }

        /// 卡内 ⑤ 性质:std 读文件与内存规格对偶(全长度域,含 ≤2 窗口整读分支
        /// 与 >2 窗口头尾分支;实读长度 = 文件长)。
        #[test]
        fn prop_sample_local_file_matches_spec(
            content in prop::collection::vec(any::<u8>(), 0..=200_000),
            tag in "[a-z]{6}",
        ) {
            let path = std::env::temp_dir().join(format!(
                "pv-checksum-prop-{}-{tag}",
                std::process::id()
            ));
            std::fs::write(&path, &content).expect("temp write");
            let len = content.len() as u64;
            let n = SAMPLE_WINDOW_BYTES as usize;
            let (head, tail) = if len <= SAMPLE_WINDOW_BYTES * 2 {
                (content.as_slice(), &[][..])
            } else {
                (&content[..n], &content[content.len() - n..])
            };
            let (read_len, fp) = sample_local_file(&path).expect("sample");
            prop_assert_eq!(read_len, len);
            prop_assert_eq!(fp, sample_fingerprint(head, tail, len));
            let _ = std::fs::remove_file(&path);
        }
    }

    /// 经公共 API 造一枚 done 行(submit → poll finished/success)。
    fn seeded_done_job(manager: &JobManager) -> String {
        let submit = FakeScriptDispatch::scripted(&[json!({ "jobid": 901 })]);
        let job = manager
            .submit(
                &submit,
                "sync/copy",
                "copy",
                "a:f.bin",
                "b:f.bin",
                &json!({}),
            )
            .expect("submit");
        let finish = FakeScriptDispatch::scripted(&[json!({
            "finished": true, "success": true, "error": "", "output": {}
        })]);
        let (record, _) = manager.poll(&finish, &job.id).expect("poll to done");
        assert_eq!(record.status, JobStatus::Done);
        job.id
    }

    #[test]
    fn verify_job_hash_tier_sets_checksum_and_roundtrips() {
        // 卡内 ④⑤:hash 档通过 → checksum 列落 `<type>:<value>`;operations/list
        // 参数形状 = 实测锚定({fs, remote, opt.hashTypes})。
        let path =
            std::env::temp_dir().join(format!("pv-checksum-verify1-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let manager = JobManager::open_at(&path).expect("store");
        let id = seeded_done_job(&manager);
        let dispatch = FakeScriptDispatch::scripted(&[
            json!({ "list": [anchored_item("other.bin", 1, json!({})),
                             anchored_item("f.bin", 24, json!({"sha256": "AAA"}))] }),
            json!({ "list": [anchored_item("f.bin", 24, json!({"sha256": "AAA"}))] }),
        ]);
        let outcome = verify_job(
            &manager,
            &dispatch,
            &id,
            &VerifyTargets {
                src_fs_base: "pvsrc:",
                src_parent: "",
                src_name: "f.bin",
                dst_fs_base: "pvdst:",
                dst_parent: "sub",
                dst_name: "f.bin",
            },
        )
        .expect("verify");
        assert_eq!(
            outcome,
            VerifyOutcome::Verified {
                tier: VerifyTier::Hash,
                checksum: Some("sha256:AAA".into())
            }
        );
        assert_eq!(
            manager.get(&id).expect("row").checksum.as_deref(),
            Some("sha256:AAA")
        );
        // 两次 rc 调用形状(双端各一次,hashTypes 候选集)。
        let calls = dispatch.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "operations/list");
        assert_eq!(
            calls[0].1,
            json!({"fs": "pvsrc:", "remote": "",
                   "opt": {"hashTypes": ["sha256", "sha1", "md5", "crc32"]}})
        );
        assert_eq!(calls[1].1["remote"], "sub");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn verify_job_sampled_tier_and_failure_landing() {
        // 卡内 ④⑤:双端本地采样通过 → 落采样标注;hash 差异 → error/fatal 终态;
        // size 差异 → error/retryable;dst 条目缺失 → error/fatal;非 done 行 Fatal。
        // 本地盘侧(fs 基座以 `/` 开头)用真实临时目录(std 读)。
        let path =
            std::env::temp_dir().join(format!("pv-checksum-verify2-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let manager = JobManager::open_at(&path).expect("store");
        let body = b"verify sampled tier body".repeat(8);
        let src_dir = std::env::temp_dir().join(format!("pv-verify-src-{}", std::process::id()));
        let dst_dir = std::env::temp_dir().join(format!("pv-verify-dst-{}", std::process::id()));
        std::fs::create_dir_all(&src_dir).expect("src dir");
        std::fs::create_dir_all(&dst_dir).expect("dst dir");
        std::fs::write(src_dir.join("f.bin"), &body).expect("src file");
        std::fs::write(dst_dir.join("f.bin"), &body).expect("dst file");
        let sample = sample_local_file(&src_dir.join("f.bin")).expect("sample").1;
        let src_base = src_dir.to_str().expect("utf8 temp dir").to_owned();
        let dst_base = dst_dir.to_str().expect("utf8 temp dir").to_owned();
        let targets = VerifyTargets {
            src_fs_base: &src_base,
            src_parent: "",
            src_name: "f.bin",
            dst_fs_base: &dst_base,
            dst_parent: "",
            dst_name: "f.bin",
        };
        // ① 采样档通过:无共同哈希 + size 相等 + 双端本地。
        let id = seeded_done_job(&manager);
        let dispatch = FakeScriptDispatch::scripted(&[
            json!({ "list": [anchored_item("f.bin", body.len() as i64, json!({"md5": "src-only"}))] }),
            json!({ "list": [anchored_item("f.bin", body.len() as i64, json!({"sha1": "dst-only"}))] }),
        ]);
        let outcome = verify_job(&manager, &dispatch, &id, &targets).expect("verify");
        assert_eq!(
            outcome,
            VerifyOutcome::Verified {
                tier: VerifyTier::Sampled,
                checksum: Some(sample.clone())
            }
        );
        assert_eq!(
            manager.get(&id).expect("row").checksum.as_deref(),
            Some(sample.as_str())
        );
        // ② hash 差异:done → error/fatal,detail 入 error 列,checksum 列不动。
        let id2 = seeded_done_job(&manager);
        let dispatch = FakeScriptDispatch::scripted(&[
            json!({ "list": [anchored_item("f.bin", 24, json!({"sha1": "s1"}))] }),
            json!({ "list": [anchored_item("f.bin", 24, json!({"sha1": "s2"}))] }),
        ]);
        let outcome = verify_job(&manager, &dispatch, &id2, &targets).expect("verify");
        assert!(matches!(
            outcome,
            VerifyOutcome::Failed {
                severity: Sev::Fatal,
                ..
            }
        ));
        let row = manager.get(&id2).expect("row");
        assert_eq!(row.status, JobStatus::Error);
        assert_eq!(row.severity, Some(Sev::Fatal));
        assert_eq!(
            row.error.as_deref(),
            Some("checksum mismatch (sha1): src=s1 dst=s2")
        );
        assert_eq!(row.checksum, None);
        // ③ size 差异:error/retryable。
        let id3 = seeded_done_job(&manager);
        let dispatch = FakeScriptDispatch::scripted(&[
            json!({ "list": [anchored_item("f.bin", 100, json!({}))] }),
            json!({ "list": [anchored_item("f.bin", 90, json!({}))] }),
        ]);
        let outcome = verify_job(&manager, &dispatch, &id3, &targets).expect("verify");
        assert!(matches!(
            outcome,
            VerifyOutcome::Failed {
                severity: Sev::Retryable,
                ..
            }
        ));
        let row = manager.get(&id3).expect("row");
        assert_eq!(
            (row.status, row.severity),
            (JobStatus::Error, Some(Sev::Retryable))
        );
        // ④ dst 条目缺失(目录在、名不在)→ error/fatal。
        let id4 = seeded_done_job(&manager);
        let dispatch = FakeScriptDispatch::scripted(&[
            json!({ "list": [anchored_item("f.bin", 24, json!({}))] }),
            json!({ "list": [] }),
        ]);
        let outcome = verify_job(&manager, &dispatch, &id4, &targets).expect("verify");
        assert!(matches!(
            outcome,
            VerifyOutcome::Failed {
                severity: Sev::Fatal,
                ..
            }
        ));
        assert!(
            manager
                .get(&id4)
                .expect("row")
                .error
                .as_deref()
                .is_some_and(|text| text.starts_with("entry not found after transfer (dst)"))
        );
        // ⑤ 非 done 行(queued)→ Fatal,零引擎触达。
        let parked = manager.register_queued("copy", "a", "b").expect("parked");
        let probe = FakeScriptDispatch::scripted(&[]);
        let err = verify_job(&manager, &probe, &parked.id, &targets).expect_err("non-done");
        assert_eq!(err.severity, Sev::Fatal);
        assert!(kind_of(&err).is_some(), "结构化 Invalid");
        assert_eq!(probe.call_count(), 0);
        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&dst_dir);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn verify_job_sampling_io_failure_lands_error_terminal() {
        // 卡内 ④(对抗审查修复项):校验通道自身采样读 IO 失败(sample_local_file
        // NotFound → classify_io = Retryable)不留 done——经 mark_verify_failed 落
        // error 终态,模块头契约与 verify_job 编排行为一致。覆盖两条可达路径:
        // ① 双端本地采样裁决分支;② 单端本地标注分支(Incomparable 路径的
        // sample_annotation 同源 IO)。
        let path =
            std::env::temp_dir().join(format!("pv-checksum-verify3-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let manager = JobManager::open_at(&path).expect("store");
        // 幽灵基座:目录不存在 → open 即 NotFound(Retryable,classify_io 表驱动)。
        let ghost_base = format!(
            "{}/pv-checksum-ghost-{}",
            std::env::temp_dir().to_str().expect("utf8 temp dir"),
            std::process::id()
        );
        // ① 双端本地 + size 缺失(无共同哈希)→ 采样裁决,src 打开即 IO 失败。
        let id = seeded_done_job(&manager);
        let targets = VerifyTargets {
            src_fs_base: &ghost_base,
            src_parent: "",
            src_name: "f.bin",
            dst_fs_base: &ghost_base,
            dst_parent: "",
            dst_name: "f.bin",
        };
        let dispatch = FakeScriptDispatch::scripted(&[
            json!({ "list": [anchored_item("f.bin", -1, json!({"md5": "src-only"}))] }),
            json!({ "list": [anchored_item("f.bin", -1, json!({"sha1": "dst-only"}))] }),
        ]);
        let outcome = verify_job(&manager, &dispatch, &id, &targets).expect("verify");
        assert!(
            matches!(
                outcome,
                VerifyOutcome::Failed {
                    severity: Sev::Retryable,
                    ..
                }
            ),
            "采样 IO 失败 = Failed(Retryable),不外浮"
        );
        let row = manager.get(&id).expect("row");
        assert_eq!(row.status, JobStatus::Error, "error 终态,不留 done");
        assert_eq!(row.severity, Some(Sev::Retryable));
        assert!(
            row.error
                .as_deref()
                .is_some_and(|text| text.starts_with("verification sampling io failed")),
            "error 列 = 采样 IO 细节,实测 {}",
            row.error.as_deref().unwrap_or("<none>")
        );
        assert_eq!(row.checksum, None, "失败不留「通过」标注");
        // ② 单端本地(size 缺失)→ Incomparable 分支的标注采样同源 IO,同路落终态。
        let id2 = seeded_done_job(&manager);
        let mixed_targets = VerifyTargets {
            src_fs_base: &ghost_base,
            src_parent: "",
            src_name: "f.bin",
            dst_fs_base: "pvdst:",
            dst_parent: "",
            dst_name: "f.bin",
        };
        let dispatch = FakeScriptDispatch::scripted(&[
            json!({ "list": [anchored_item("f.bin", -1, json!({"md5": "src-only"}))] }),
            json!({ "list": [anchored_item("f.bin", -1, json!({"sha1": "dst-only"}))] }),
        ]);
        let outcome = verify_job(&manager, &dispatch, &id2, &mixed_targets).expect("verify");
        assert!(matches!(
            outcome,
            VerifyOutcome::Failed {
                severity: Sev::Retryable,
                ..
            }
        ));
        let row = manager.get(&id2).expect("row");
        assert_eq!(row.status, JobStatus::Error);
        assert_eq!(row.severity, Some(Sev::Retryable));
        let _ = std::fs::remove_file(&path);
    }
}
