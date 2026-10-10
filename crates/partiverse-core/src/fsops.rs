//! 文件操作编排(M1-WP06-T02 卡内 DoD ①②):同步 fs 操作(mkdir/delete,预算
//! acquire 前置、错误 Severity 原样上浮)+ 单文件/跨 Node 传输编排(gated_submit
//! 全链)+ 进度聚合(job 轮询顺带 `core/stats` 组采样 → T01 progress 列落库)。
//!
//! rc 形状全部 2026-10-10 本机 rclone 1.75.1 实测锚定(运行报告 §锚定取证,
//! 禁凭记忆):`operations/mkdir`/`operations/delete` 参数 = `{fs:"remote:path"}`
//!(delete 语义 = 删除该目录树下全部文件、保留目录壳;fs 指向文件 → Fatal
//! "is a file not a directory",引擎白名单无单文件删除命令);`operations/copyfile`/
//! `movefile` 参数 = `{srcFs,srcRemote,dstFs,dstRemote}`(1.75.1 用 srcRemote);
//! rc `sync/copy|move` 的 srcFs 指向**文件**必失败("readdirent … not a directory")
//! ——「sync/copy 单文件语义」唯一可行形态 = 源父目录 + 过滤器
//! `_filter:{FilterRule:["+ <name>","- *"]}`(实测只拷目标文件);`core/stats`
//! 支持 `{group:"job/<jobid>"}` 每 job 独立统计(未知组 = 全零对象)。
//!
//! 单文件路径协商(卡内 ①):显式 caps 且 src/dst 双端声明 `server_side_copy`
//! → `operations/copyfile|movefile`(同步服务端操作,可携带重命名目标);
//! 无 caps / 未声明 → sync/copy|move + 过滤器保守路径(P9:Default 全无;
//! caps 采集属 WP09 `fsinfo`/`backend features`,本卡不发明 caps 来源)。
//! Windows 限制(卡内 ⑦):rc 通道 unix socket 专属,非 unix 平台 rc 调用 =
//! Fatal(ADR-0005 / engine client PlatformUnsupported),本模块全部能力在
//! Windows 不可用——如实登记,三端验收范围待 Owner 对齐,非静默。
//!
//! # 预算语义
//! 同步操作 acquire 前置:Allow → 执行;Throttled/Exhausted → 零引擎触达原样
//! 返回;执行失败 → `settle(Refund)` 退回预留再上浮(与 budget error 退回
//! V1 骨架策略同口径)。传输走 `BudgetScheduler::gated_submit` 全链(Exhausted
//! 停放 queued 零触达,复活经 `JobManager::revive_parked`,重试退避执行点 =
//! 编排层,core 零 sleep)。

use serde_json::Value;

use crate::budget::{BudgetScheduler, GatedSubmit, JobSpec, Metering};
use crate::caps::ProviderCaps;
use crate::error::PartisyError;
use crate::jobs::{JobErrorKind, JobManager, JobRecord, JobStatus, RcDispatch};

/// 同步文件操作结果(卡内 ①:预算门三态原样上浮,零吞错)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsOpOutcome {
    /// 已执行(rc 调用成功返回)。
    Applied,
    /// 预算限流,可等待(零引擎触达;wait_until_ms = 可重试时刻)。
    Throttled { wait_until_ms: u64 },
    /// 预算结构性拒绝(cost 超桶容;cost=1 时不可达,形状完备保留)。
    Exhausted,
}

/// fs 串拼接:`base` 以 `:`/`/` 结尾直接续接,否则补 `/`(`"pvsrc:"` + `"sub"`
/// → `"pvsrc:sub"`;本地裸路径 `"/tmp/x"` + `"d"` → `"/tmp/x/d"`)。
#[must_use]
pub fn compose_fs(base: &str, path: &str) -> String {
    if path.is_empty() {
        return base.to_owned();
    }
    if base.ends_with(':') || base.ends_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// remote 相对路径的父目录(`"a/b.txt"` → `"a"`;根级文件 → `""`;零段 → None)。
#[must_use]
fn parent_remote(remote: &str) -> Option<&str> {
    let trimmed = remote.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rsplit_once('/') {
        Some((parent, _)) => Some(parent),
        None => Some(""),
    }
}

/// 单文件 sync 过滤器(1.75.1 实测锚定形状):精确选中源文件名、排除其余。
/// 规则序有意义(首中即停),`- *` 兜底排除,禁单条 `+` 规则(实测空结果)。
#[must_use]
fn single_file_filter(name: &str) -> Value {
    serde_json::json!({ "FilterRule": [format!("+ /{name}"), "- *"] })
}

/// 同步文件操作单源(mkdir/delete 共用):acquire 前置 → Allow 执行 rc →
/// 失败退预留再上浮(零吞错,severity 由 dispatch 原样保真)。
fn sync_fs_op(
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    node: &str,
    method: &str,
    params: &Value,
    cost: u64,
) -> Result<FsOpOutcome, PartisyError> {
    match budget.acquire(node, cost)? {
        crate::budget::Decision::Allow => {}
        crate::budget::Decision::Throttled { wait_until_ms } => {
            return Ok(FsOpOutcome::Throttled { wait_until_ms });
        }
        crate::budget::Decision::Exhausted => return Ok(FsOpOutcome::Exhausted),
    }
    if let Err(err) = dispatch.call(method, params) {
        // rc 失败:退回已扣预留再上浮(预算 V1 骨架策略:error 退回)。
        budget.settle(node, Metering::Refund(cost))?;
        return Err(err);
    }
    Ok(FsOpOutcome::Applied)
}

/// 新建目录(卡内 ①):`fs` = remote 基座(`"remote:"` 或本地裸路径),`remote`
/// = 基座内目标路径(嵌套段实测可一次建齐);语义 = rc `operations/mkdir`
/// (1.75.1 实测**双参数必填** `{fs, remote}`,fs 携带路径会被 400 拒绝)。
///
/// # Errors
/// 预算决策面/SQLite 面/severity 保真的 rc 失败均结构化上浮。
pub fn fs_mkdir(
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    node: &str,
    fs: &str,
    remote: &str,
    cost: u64,
) -> Result<FsOpOutcome, PartisyError> {
    sync_fs_op(
        budget,
        dispatch,
        node,
        "operations/mkdir",
        &serde_json::json!({ "fs": fs, "remote": remote }),
        cost,
    )
}

/// 删除目录树文件(卡内 ①):`fs` = 目标目录全路径;实测语义 = 递归删除该目录
/// 树下**全部文件、保留目录壳**(rc `operations/delete`;引擎白名单无单文件
/// 删除命令,fs 指向文件 → 实测 500 原样上浮)。UI 必须预览-提交并如实展示本语义
/// (破坏性操作禁直通)。
///
/// # Errors
/// 同 [`fs_mkdir`]。
pub fn fs_delete(
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    node: &str,
    fs: &str,
    cost: u64,
) -> Result<FsOpOutcome, PartisyError> {
    sync_fs_op(
        budget,
        dispatch,
        node,
        "operations/delete",
        // 实测:delete 接受 fs 携带路径(`"remote:path"`),remote 键无关。
        &serde_json::json!({ "fs": fs }),
        cost,
    )
}

/// 单文件传输路径协商结论(卡内 ①)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTransferPlan {
    /// 双端声明 server_side_copy:同步 `operations/copyfile|movefile`
    /// (支持 dstRemote 重命名;零 async job)。
    ServerSide { method: &'static str },
    /// 无 caps / 未声明(保守,P9):async `sync/copy|move` + 单文件过滤器
    /// (同名落盘;重命名在本路径不可表达,UI 侧按显式禁用处理,禁静默)。
    SyncJob,
}

/// 单文件路径协商(卡内 ①):src/dst **双端**都显式声明 `server_side_copy`
/// 才走服务端单文件操作(保守交集);任一端无 caps / 未声明 → 过滤器路径。
#[must_use]
pub fn single_file_plan(
    src: Option<ProviderCaps>,
    dst: Option<ProviderCaps>,
    is_move: bool,
) -> FileTransferPlan {
    let ok = |caps: Option<ProviderCaps>| caps.is_some_and(|c| c.server_side_copy);
    if ok(src) && ok(dst) {
        FileTransferPlan::ServerSide {
            method: if is_move {
                "operations/movefile"
            } else {
                "operations/copyfile"
            },
        }
    } else {
        FileTransferPlan::SyncJob
    }
}

/// 跨 Node 传输规格(gated_submit 全链入参;kind = 行内展示元数据)。
#[derive(Debug, Clone, Copy)]
pub struct TransferSpec<'a> {
    /// 预算计量 Node 键(provider profile 键;无 profile = 不限流)。
    pub node: &'a str,
    /// 预算请求成本(请求频率单位;字节限速下沉 core/bwlimit,非本面)。
    pub cost: u64,
    /// true = sync/move,false = sync/copy。
    pub is_move: bool,
    /// 行内种类元数据(如 "copy"/"move";非空,JobManager 校验)。
    pub kind: &'a str,
    /// 源 fs(srcFs;目录树或源父目录)。
    pub src_fs: &'a str,
    /// 目标 fs(dstFs;目录)。
    pub dst_fs: &'a str,
}

/// 跨 Node sync/copy|move 编排(卡内 ① gated_submit 全链):method 择定 →
/// `BudgetScheduler::gated_submit`(Allow=提交引擎;Throttled=零落行;
/// Exhausted=停放 queued 零触达)。
///
/// # Errors
/// 预算/JobManager/severity 保真 rc 失败结构化上浮。
pub fn transfer_submit(
    manager: &JobManager,
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    spec: &TransferSpec<'_>,
) -> Result<GatedSubmit, PartisyError> {
    let method = if spec.is_move {
        "sync/move"
    } else {
        "sync/copy"
    };
    let params = serde_json::json!({ "srcFs": spec.src_fs, "dstFs": spec.dst_fs });
    budget.gated_submit(
        manager,
        dispatch,
        spec.node,
        spec.cost,
        JobSpec {
            method,
            kind: spec.kind,
            src: spec.src_fs,
            dst: spec.dst_fs,
            params: &params,
        },
    )
}

/// 单文件传输结果(ServerSide 同步执行或 SyncJob 入队;预算三态保真)。
#[derive(Debug, Clone, PartialEq)]
pub enum SingleFileOutcome {
    /// 服务端单文件操作已同步执行完成(含重命名目标)。
    SyncApplied,
    /// 已入引擎(async job,见 [`GatedSubmit::Submitted`] 同语义)。
    Submitted(JobRecord),
    /// 预算限流零落行,可等待重试。
    Throttled { wait_until_ms: u64 },
    /// 预算结构性拒绝,停放 queued 零引擎触达。
    Exhausted(JobRecord),
}

/// 单文件传输规格([`single_file_transfer`] 入参打包,参数结构防参序错置)。
#[derive(Debug, Clone, Copy)]
pub struct SingleFileSpec<'a> {
    /// 预算计量 Node 键(无 profile = 不限流)。
    pub node: &'a str,
    /// 预算请求成本。
    pub cost: u64,
    /// true = move(movefile/sync/move),false = copy。
    pub is_move: bool,
    /// 行内种类元数据(如 "copy"/"move";非空,JobManager 校验)。
    pub kind: &'a str,
    /// 源 fs 基座(`"remote:"` 或本地裸路径;remote 相对其解析)。
    pub src_fs_base: &'a str,
    /// 源文件相对路径(如 "sub/a.txt")。
    pub src_remote: &'a str,
    /// 目标目录 fs(SyncJob 落盘目录/ServerSide 的 dstFs)。
    pub dst_fs: &'a str,
    /// 目标相对路径(仅 ServerSide 可携重命名;SyncJob 同名落盘)。
    pub dst_remote: &'a str,
    /// 源端 caps(协商输入;None = 未采集,P9 保守)。
    pub src_caps: Option<ProviderCaps>,
    /// 目标端 caps(同上)。
    pub dst_caps: Option<ProviderCaps>,
}

/// 单文件传输编排(卡内 ①):协商 [`FileTransferPlan`]——ServerSide →
/// acquire 前置 + `operations/copyfile|movefile`(dst_remote 支持重命名;
/// 失败退预留再上浮);SyncJob → 源父目录 + 单文件过滤器经 gated_submit
/// 入队(dst_remote 仅取其父目录,文件名随源;重命名不可表达)。
///
/// # Errors
/// 同 [`transfer_submit`];remote 路径不可解析(空)→ 结构化 Fatal。
pub fn single_file_transfer(
    manager: &JobManager,
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    spec: &SingleFileSpec<'_>,
) -> Result<SingleFileOutcome, PartisyError> {
    let SingleFileSpec {
        node,
        cost,
        is_move,
        kind,
        src_fs_base,
        src_remote,
        dst_fs,
        dst_remote,
        src_caps,
        dst_caps,
    } = *spec;
    let name = src_remote
        .trim_end_matches('/')
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(src_remote.trim_end_matches('/'));
    if name.is_empty() {
        return Err(JobErrorKind::Invalid(format!(
            "single file transfer: src_remote has no file name: {src_remote:?}"
        ))
        .fatal());
    }
    match single_file_plan(src_caps, dst_caps, is_move) {
        FileTransferPlan::ServerSide { method } => {
            match budget.acquire(node, cost)? {
                crate::budget::Decision::Throttled { wait_until_ms } => {
                    return Ok(SingleFileOutcome::Throttled { wait_until_ms });
                }
                crate::budget::Decision::Exhausted => {
                    let parked = manager.register_queued(kind, src_remote, dst_remote)?;
                    return Ok(SingleFileOutcome::Exhausted(parked));
                }
                crate::budget::Decision::Allow => {}
            }
            let params = serde_json::json!({
                "srcFs": src_fs_base,
                "srcRemote": src_remote,
                "dstFs": dst_fs,
                "dstRemote": dst_remote,
            });
            if let Err(err) = dispatch.call(method, &params) {
                budget.settle(node, Metering::Refund(cost))?;
                return Err(err);
            }
            Ok(SingleFileOutcome::SyncApplied)
        }
        FileTransferPlan::SyncJob => {
            let Some(parent) = parent_remote(src_remote) else {
                return Err(JobErrorKind::Invalid(format!(
                    "single file transfer: src_remote unresolvable: {src_remote:?}"
                ))
                .fatal());
            };
            // 单文件过滤器语义:srcFs = 源父目录,dstFs = 目标目录(同名落盘)。
            let src_parent_fs = compose_fs(src_fs_base, parent);
            let mut params = serde_json::json!({ "srcFs": src_parent_fs, "dstFs": dst_fs });
            params["_filter"] = single_file_filter(name);
            let method = if is_move { "sync/move" } else { "sync/copy" };
            match budget.gated_submit(
                manager,
                dispatch,
                node,
                cost,
                JobSpec {
                    method,
                    kind,
                    src: src_remote,
                    dst: dst_remote,
                    params: &params,
                },
            )? {
                GatedSubmit::Submitted(record) => Ok(SingleFileOutcome::Submitted(record)),
                GatedSubmit::Throttled { wait_until_ms } => {
                    Ok(SingleFileOutcome::Throttled { wait_until_ms })
                }
                GatedSubmit::Exhausted(record) => Ok(SingleFileOutcome::Exhausted(record)),
            }
        }
    }
}

/// 从 `core/stats` 组载荷提取 (已传字节, 预估总字节)(卡内 ②,纯函数)。
/// `bytes`/`totalBytes` 缺失 = None(设计态:作业未起/listing 阶段,禁造值);
/// 存在但非非负整数 = 协议损坏 Fatal(零猜测);返回 None 时不落进度列。
///
/// # Errors
/// 载荷非对象或字段损坏 → Fatal。
pub fn extract_progress(stats: &Value) -> Result<Option<(u64, Option<u64>)>, PartisyError> {
    let Some(obj) = stats.as_object() else {
        return Err(JobErrorKind::Invalid(format!(
            "core/stats: payload is not an object: {stats}"
        ))
        .fatal());
    };
    let field = |key: &str| -> Result<Option<u64>, PartisyError> {
        match obj.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value.as_u64().map(Some).ok_or_else(|| {
                JobErrorKind::Invalid(format!(
                    "core/stats: `{key}` is not a non-negative integer: {value}"
                ))
                .fatal()
            }),
        }
    };
    let bytes = field("bytes")?;
    let total = field("totalBytes")?;
    Ok(match (bytes, total) {
        (None, None) => None,
        (bytes, total) => Some((bytes.unwrap_or(0), total)),
    })
}

/// 轮询 + 进度聚合(卡内 ②):先 `JobManager::poll`(主面,状态机推进/终态
/// 裁决,错误零吞);仅对 running 且带锚行顺带 `core/stats` 组采样(终态/停放
/// 行零引擎触达,与 poll 同口径)并落 T01 progress 列,返回重读后的最新记录。
/// 采样缺失(None)不落列(设计态);采样调用失败结构化上浮(禁静默降级)。
///
/// # Errors
/// poll/stats/落库任一失败 → severity 保真上浮。
pub fn poll_with_progress(
    manager: &JobManager,
    dispatch: &impl RcDispatch,
    id: &str,
) -> Result<(JobRecord, Option<Value>), PartisyError> {
    let (record, output) = manager.poll(dispatch, id)?;
    if let (JobStatus::Running, Some(jobid)) = (record.status, record.engine_job_id) {
        let reply = dispatch.call(
            "core/stats",
            &serde_json::json!({ "group": format!("job/{jobid}") }),
        )?;
        if let Some((bytes, total)) = extract_progress(&reply)? {
            manager.update_progress(&record.id, bytes, total)?;
        }
    }
    Ok((manager.get(id)?, output))
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque};

    use super::*;
    use crate::budget::{Decision, QuotaProfiles};
    use crate::error::Severity;
    use crate::jobs::kind_of;
    use serde_json::json;

    /// 预算夹具(cap=2,窗口=1000s;数值非 provider 事实)。
    const PROFILES: &str = r#"{"version":1,"profiles":{"testprov":{"requests_per_window":2,"window_secs":1000,"source":"test fixture"}},"bwlimit":{"rate":"off"},"backoff_secs":60}"#;

    fn budget(path: &std::path::Path) -> BudgetScheduler {
        BudgetScheduler::open_at(
            path,
            QuotaProfiles::parse(PROFILES).expect("fixture parses"),
        )
        .expect("open budget")
    }

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

    #[test]
    fn compose_fs_and_parent_and_filter_shapes_hold() {
        // fs 拼接(remote:/本地裸路径/空段);父目录解析;过滤器实测形状。
        assert_eq!(compose_fs("pvsrc:", "sub"), "pvsrc:sub");
        assert_eq!(compose_fs("pvsrc:", ""), "pvsrc:");
        assert_eq!(compose_fs("/tmp/x", "d"), "/tmp/x/d");
        assert_eq!(compose_fs("pvsrc:root/", "d"), "pvsrc:root/d");
        assert_eq!(parent_remote("a/b.txt"), Some("a"));
        assert_eq!(parent_remote("b.txt"), Some(""));
        assert_eq!(parent_remote(""), None);
        assert_eq!(
            single_file_filter("b.txt"),
            json!({ "FilterRule": ["+ /b.txt", "- *"] })
        );
    }

    #[test]
    fn single_file_plan_requires_both_ends_declared() {
        // 卡内 ① 保守交集:双端显式声明才走服务端;单端/无 caps → SyncJob;
        // move 择 movefile,copy 择 copyfile。
        let caps = || {
            Some(ProviderCaps {
                server_side_copy: true,
                ..ProviderCaps::default()
            })
        };
        let no = Some(ProviderCaps::default());
        assert_eq!(
            single_file_plan(caps(), caps(), false),
            FileTransferPlan::ServerSide {
                method: "operations/copyfile"
            }
        );
        assert_eq!(
            single_file_plan(caps(), caps(), true),
            FileTransferPlan::ServerSide {
                method: "operations/movefile"
            }
        );
        assert_eq!(
            single_file_plan(caps(), None, false),
            FileTransferPlan::SyncJob
        );
        assert_eq!(
            single_file_plan(no, caps(), false),
            FileTransferPlan::SyncJob
        );
        assert_eq!(
            single_file_plan(None, None, true),
            FileTransferPlan::SyncJob
        );
    }

    #[test]
    fn sync_fs_ops_gate_dispatch_and_refund_on_failure() {
        // 卡内 ①:Allow → dispatch({fs} 形状实测锚定);Throttled 零触达;
        // rc 失败 → 退预留 + severity 保真上浮。
        let path =
            std::env::temp_dir().join(format!("partiverse-fsops-unit-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let sched = budget(&path);
        let manager = JobManager::open_at(&path).expect("jobs store");
        let _ = manager; // 同库联迁证明(迁移序列单源)
        let ok = FakeScriptDispatch::scripted(&[json!({}), json!({})]);
        assert_eq!(
            fs_mkdir(&sched, &ok, "testprov", "pvsrc:", "newdir", 1).expect("mkdir"),
            FsOpOutcome::Applied
        );
        assert_eq!(
            ok.calls.borrow()[0].clone(),
            (
                "operations/mkdir".into(),
                json!({ "fs": "pvsrc:", "remote": "newdir" })
            )
        );
        // 第二次 Allow 耗尽桶容(2/2),第三次 = Throttled(窗口未滚),零触达。
        assert_eq!(
            fs_delete(&sched, &ok, "testprov", "pvsrc:old", 1).expect("delete"),
            FsOpOutcome::Applied
        );
        let probe = FakeScriptDispatch::scripted(&[]);
        let throttled = fs_delete(&sched, &probe, "testprov", "pvsrc:older", 1).expect("throttled");
        assert!(
            matches!(throttled, FsOpOutcome::Throttled { wait_until_ms } if wait_until_ms > 1_700_000_000_000),
            "wait_until_ms 须为未来时刻(真实时钟窗口尾): {throttled:?}"
        );
        assert_eq!(probe.calls.borrow().len(), 0, "Throttled 零引擎触达");
        // 新桶(cap=2):首次执行失败 → Refund 后上浮,随后同额可再取(退回实证)。
        let local_budget = budget(
            &std::env::temp_dir().join(format!("partiverse-fsops-unit2-{}.db", std::process::id())),
        );
        let failing = FakeScriptDispatch::scripted(&[]);
        let err = fs_delete(&local_budget, &failing, "testprov", "pvsrc:x", 1)
            .expect_err("script exhausted surfaces");
        assert_eq!(err.severity, Severity::Fatal);
        assert_eq!(
            local_budget.acquire("testprov", 2).expect("acquire"),
            Decision::Allow,
            "失败已退预留(1 已退 + 余 1 = 可取 2)"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn transfer_submit_assembles_gated_full_chain() {
        // 卡内 ①:gated_submit 全链——Exhausted 停放 queued 零触达;Allow 提交
        // (method/params 形状实测锚定);Throttled 零落行。
        let path =
            std::env::temp_dir().join(format!("partiverse-fsops-unit3-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let sched = budget(&path);
        let manager = JobManager::open_at(&path).expect("jobs store");
        let spec = TransferSpec {
            node: "testprov",
            cost: 5,
            is_move: true,
            kind: "move",
            src_fs: "a:",
            dst_fs: "b:",
        };
        let untouched = FakeScriptDispatch::scripted(&[]);
        let exhausted =
            match transfer_submit(&manager, &sched, &untouched, &spec).expect("exhausted parks") {
                GatedSubmit::Exhausted(record) => record,
                other => panic!("expected Exhausted, got {other:?}"),
            };
        assert_eq!(untouched.calls.borrow().len(), 0, "Exhausted 零触达");
        assert_eq!(exhausted.status, JobStatus::Queued);
        assert!(exhausted.engine_job_id.is_none());
        let small = TransferSpec { cost: 1, ..spec };
        let submitted = FakeScriptDispatch::scripted(&[json!({ "jobid": 7 })]);
        let job = match transfer_submit(&manager, &sched, &submitted, &small).expect("submitted") {
            GatedSubmit::Submitted(record) => record,
            other => panic!("expected Submitted, got {other:?}"),
        };
        assert_eq!(job.engine_job_id, Some(7));
        let (method, params) = submitted.calls.borrow()[0].clone();
        assert_eq!(method, "sync/move");
        assert_eq!(
            params,
            json!({ "srcFs": "a:", "dstFs": "b:", "_async": true })
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn single_file_transfer_syncjob_uses_parent_filter_and_rejects_bad_remote() {
        // 卡内 ①(无 caps 保守路径):srcFs=父目录 + FilterRule 单文件过滤器
        //(实测锚定形状);空 remote 名 = Fatal;ServerSide 形状另测。
        let path =
            std::env::temp_dir().join(format!("partiverse-fsops-unit4-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let sched = budget(&path);
        let manager = JobManager::open_at(&path).expect("jobs store");
        let submitted = FakeScriptDispatch::scripted(&[json!({ "jobid": 9 })]);
        let outcome = single_file_transfer(
            &manager,
            &sched,
            &submitted,
            &SingleFileSpec {
                node: "testprov",
                cost: 1,
                is_move: false,
                kind: "copy",
                src_fs_base: "pvsrc:",
                src_remote: "sub/b.txt",
                dst_fs: "pvdst:",
                dst_remote: "b.txt",
                src_caps: None,
                dst_caps: None,
            },
        )
        .expect("syncjob single file");
        assert!(matches!(outcome, SingleFileOutcome::Submitted(_)));
        let (method, params) = submitted.calls.borrow()[0].clone();
        assert_eq!(method, "sync/copy");
        assert_eq!(
            params,
            json!({
                "srcFs": "pvsrc:sub",
                "dstFs": "pvdst:",
                "_filter": { "FilterRule": ["+ /b.txt", "- *"] },
                "_async": true,
            })
        );
        let err = single_file_transfer(
            &manager,
            &sched,
            &submitted,
            &SingleFileSpec {
                node: "testprov",
                cost: 1,
                is_move: false,
                kind: "copy",
                src_fs_base: "pvsrc:",
                src_remote: "",
                dst_fs: "pvdst:",
                dst_remote: "x",
                src_caps: None,
                dst_caps: None,
            },
        )
        .expect_err("empty remote rejected");
        assert_eq!(err.severity, Severity::Fatal);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn single_file_transfer_serverside_executes_sync_with_rename() {
        // 卡内 ①(双端 caps):同步 copyfile {srcFs,srcRemote,dstFs,dstRemote}
        //(1.75.1 实测参数名),dstRemote 携带重命名目标;失败退预留。
        let path =
            std::env::temp_dir().join(format!("partiverse-fsops-unit5-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let sched = budget(&path);
        let manager = JobManager::open_at(&path).expect("jobs store");
        let caps = || {
            Some(ProviderCaps {
                server_side_copy: true,
                ..ProviderCaps::default()
            })
        };
        let ok = FakeScriptDispatch::scripted(&[json!({})]);
        let outcome = single_file_transfer(
            &manager,
            &sched,
            &ok,
            &SingleFileSpec {
                node: "testprov",
                cost: 1,
                is_move: false,
                kind: "copy",
                src_fs_base: "pvsrc:",
                src_remote: "a/b.txt",
                dst_fs: "pvdst:",
                dst_remote: "renamed.txt",
                src_caps: caps(),
                dst_caps: caps(),
            },
        )
        .expect("serverside copy");
        assert_eq!(outcome, SingleFileOutcome::SyncApplied);
        assert_eq!(
            ok.calls.borrow()[0].clone(),
            (
                "operations/copyfile".into(),
                json!({
                    "srcFs": "pvsrc:",
                    "srcRemote": "a/b.txt",
                    "dstFs": "pvdst:",
                    "dstRemote": "renamed.txt",
                })
            )
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn extract_progress_reads_fields_and_rejects_corruption() {
        // 卡内 ②:缺键/全缺 = None(设计态);非负整数读出;负数/浮点/非对象 =
        // Fatal 协议损坏(实测组统计恒含 bytes/totalBytes 两键)。
        assert_eq!(extract_progress(&json!({})).expect("empty"), None);
        assert_eq!(
            extract_progress(&json!({ "bytes": 3, "totalBytes": 10 })).expect("read"),
            Some((3, Some(10)))
        );
        assert_eq!(
            extract_progress(&json!({ "bytes": 0, "totalBytes": 0, "speed": 1.5 })).expect("zeros"),
            Some((0, Some(0)))
        );
        assert_eq!(
            extract_progress(&json!({ "totalBytes": 5 })).expect("bytes absent"),
            Some((0, Some(5)))
        );
        for bad in [
            json!({ "bytes": -1 }),
            json!({ "bytes": 1.5 }),
            json!([]),
            json!("bytes"),
        ] {
            let err = extract_progress(&bad).expect_err("corruption surfaces");
            assert_eq!(err.severity, Severity::Fatal, "{bad}");
            assert!(matches!(kind_of(&err), Some(JobErrorKind::Invalid(_))));
        }
    }

    #[test]
    fn poll_with_progress_samples_only_anchored_running_rows() {
        // 卡内 ②:running+带锚 → 采样 core/stats {group:"job/<id>"}(实测形状)
        // 并落 progress 列;终态/停放行零引擎触达;采样损坏 Fatal 上浮。
        let path =
            std::env::temp_dir().join(format!("partiverse-fsops-unit6-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let sched = budget(&path);
        let manager = JobManager::open_at(&path).expect("jobs store");
        let submitted = FakeScriptDispatch::scripted(&[json!({ "jobid": 11 })]);
        let job = match transfer_submit(
            &manager,
            &sched,
            &submitted,
            &TransferSpec {
                node: "testprov",
                cost: 1,
                is_move: false,
                kind: "copy",
                src_fs: "a:",
                dst_fs: "b:",
            },
        )
        .expect("submit")
        {
            GatedSubmit::Submitted(record) => record,
            other => panic!("expected Submitted, got {other:?}"),
        };
        // queued 带锚行首轮 poll = 未 finished → running;同轮顺带采样。
        let sampler = FakeScriptDispatch::scripted(&[
            json!({ "finished": false, "success": false, "error": "" }),
            json!({ "bytes": 40, "totalBytes": 100 }),
        ]);
        let (record, _) = poll_with_progress(&manager, &sampler, &job.id).expect("poll 1");
        assert_eq!(record.status, JobStatus::Running);
        assert_eq!(
            (record.progress_bytes, record.progress_total),
            (Some(40), Some(100))
        );
        // 终态行:零采样触达(poll 幂等返回)。
        let finish = FakeScriptDispatch::scripted(&[
            json!({ "finished": true, "success": true, "error": "", "output": {} }),
        ]);
        let (record, _) = poll_with_progress(&manager, &finish, &job.id).expect("poll 2");
        assert_eq!(record.status, JobStatus::Done);
        // done 后 poll 零触达(终态幂等),进度列保真不丢。
        let probe = FakeScriptDispatch::scripted(&[]);
        let (record, _) = poll_with_progress(&manager, &probe, &job.id).expect("poll 3");
        assert_eq!(record.status, JobStatus::Done);
        assert_eq!(probe.calls.borrow().len(), 0);
        // parked 无锚行:零触达原样返回。
        let parked = manager.register_queued("copy", "a:", "b:").expect("parked");
        let probe = FakeScriptDispatch::scripted(&[]);
        let (record, _) = poll_with_progress(&manager, &probe, &parked.id).expect("parked poll");
        assert_eq!(record.status, JobStatus::Queued);
        assert_eq!(probe.calls.borrow().len(), 0);
        // 采样损坏(running 轮):Fatal 上浮,零吞错。
        let submitted2 = FakeScriptDispatch::scripted(&[json!({ "jobid": 12 })]);
        let job2 = match transfer_submit(
            &manager,
            &sched,
            &submitted2,
            &TransferSpec {
                node: "testprov",
                cost: 1,
                is_move: false,
                kind: "copy",
                src_fs: "a:",
                dst_fs: "b:",
            },
        )
        .expect("submit 2")
        {
            GatedSubmit::Submitted(record) => record,
            other => panic!("expected Submitted, got {other:?}"),
        };
        let corrupted = FakeScriptDispatch::scripted(&[
            json!({ "finished": false, "success": false, "error": "" }),
            json!({ "bytes": -1 }),
        ]);
        let err = poll_with_progress(&manager, &corrupted, &job2.id).expect_err("corrupt stats");
        assert_eq!(err.severity, Severity::Fatal);
        let _ = std::fs::remove_file(&path);
    }
}
