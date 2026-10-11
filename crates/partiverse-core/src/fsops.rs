//! 文件操作编排(M1-WP06-T02 卡内 DoD ①②):同步 fs 操作(mkdir/delete,预算
//! acquire 前置、错误 Severity 原样上浮)+ 单文件/跨 Node 传输编排(gated_submit
//! 全链)+ 进度聚合(job 轮询顺带 `core/stats` 组采样 → T01 progress 列落库)。
//! M1-WP06-T05 追加:stat 判型分流(typed_transfer,copy/move 自动文件/目录
//! 路由 + 空源提示)+ 判型删除路由(fs_remove:文件→delete、目录→purge 整树)。
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
//! T05 追加实机锚定(2026-10-11,Python 直驱 rcd,run 报告 §锚定取证):
//! `operations/stat` = `{fs, remote}` 双键,2xx 回 `{item:{IsDir}}`(缺失对象 =
//! 2xx + `item:null`;bare 本地文件路径作 fs 根 = 500 "is a file not a directory",
//! bare 缺失 = 404 "directory not found");`operations/delete` **忽略 remote 键**
//(T02 既有锚同证)语义 = 对 fs 树做**全量文件清除**(保留目录壳;多文件根
//! 裁决 J1:4 文件根删 remote=a.txt → 4 文件全灭)——对单文件目标是破坏性误删,
//! fs 携文件路径形状 = 500 且带删除副作用(B2),故**禁用于单文件删除**(文件
//! 路由见 [`fs_remove`] 升级裁决注);`operations/purge` = `{fs, remote}` 双键
//! 必填(缺 remote 键 = 400),目录 = 整树连壳删除,对文件 = 干净 500 零删除
//! (N1);精确单文件删除唯一可用命令 = `operations/deletefile`(K1/M1 实证,
//! Owner 升级裁决授权入白名单);`operations/copyfile` 的 dstRemote 支持嵌套
//! 路径(父目录自动创建)、尾斜杠 = 500 且带副作用(禁尾斜杠)。
//!
//! 单文件路径协商(T02 既有,卡内 ①):显式 caps 且 src/dst 双端声明
//! `server_side_copy` → `operations/copyfile|movefile`(同步服务端操作,可携带
//! 重命名目标);无 caps / 未声明 → sync/copy|move + 过滤器保守路径(P9:Default
//! 全无;caps 采集属 WP09 `fsinfo`/`backend features`)。T05 起该协商面仅服务
//! CLI `--file` 显式旗标路径;无旗标 copy/move 走 [`typed_transfer`] 自动判型
//! (文件 → copyfile/movefile 通道)。
//!
//! 判型探针(stat/list)不设预算门:与 `operations/list` 既有着面(browse/rm
//! 预览直调 dispatch)同口径,廉价元数据读;真正的传输/删除操作照旧 acquire
//! 前置。Windows 限制(卡内 ⑦):rc 通道 unix socket 专属,非 unix 平台 rc 调用 =
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

/// stat 判型结论(T05 卡内 ①):目标存在时的物理形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// 普通文件(`item.IsDir == false`,1.75.1 实锚)。
    File,
    /// 目录(`item.IsDir == true`,1.75.1 实锚;remote 根恒为目录)。
    Directory,
}

/// stat 判型(T05 卡内 ①,预飞实机实证通道):`operations/stat {fs, remote}`
/// 双键探测 → `item.IsDir`。返回 `None` = 目标不存在(实锚三形态:具名 remote
/// 2xx + `item:null`;bare 本地路径 404 "directory not found";bare 本地文件
/// 路径作 fs 根 = 500 "is a file not a directory" → 该根本身是文件,判
/// [`EntryKind::File`])。其余判型失败 = 原样上浮(零吞错,禁猜测路由)。
/// 探针不设预算门(list 既有着面,见模块文档)。
///
/// # Errors
/// rc 失败(非锚定的缺失/文件形态)或 `item.IsDir` 缺失/非布尔(协议损坏,
/// 零猜测)→ severity 保真结构化上浮。
pub fn stat_kind(
    dispatch: &impl RcDispatch,
    fs: &str,
    remote: &str,
) -> Result<Option<EntryKind>, PartisyError> {
    match dispatch.call(
        "operations/stat",
        &serde_json::json!({ "fs": fs, "remote": remote }),
    ) {
        Ok(reply) => match reply.get("item") {
            None | Some(Value::Null) => Ok(None),
            Some(item) => match item.get("IsDir").and_then(Value::as_bool) {
                Some(true) => Ok(Some(EntryKind::Directory)),
                Some(false) => Ok(Some(EntryKind::File)),
                None => Err(JobErrorKind::Invalid(format!(
                    "operations/stat: `item.IsDir` missing or not a boolean: {reply}"
                ))
                .fatal()),
            },
        },
        Err(err) => match classify_stat_error(&err) {
            Some(classified) => Ok(classified),
            // 非锚定形态:原样上浮(零吞错,禁猜测路由)。
            None => Err(err),
        },
    }
}

/// stat 判型失败的实机锚定分类(T05 预飞探针,2026-10-11 rclone 1.75.1):
/// 错误链文本命中 `is a file not a directory` → fs 根本身是文件(bare 路径
/// 文件场景);命中 `http 404` / `directory not found` → 目标缺失(404 形态,
/// 与具名 remote `item:null` 双形态互补)。未命中任何锚 = None → 调用方原样
/// 上浮(禁静默降级)。文本锚与引擎 Display 形状
/// `rc client error (operations/stat, http 404): …` 单源(2026-10-11 实锚)。
fn classify_stat_error(err: &PartisyError) -> Option<Option<EntryKind>> {
    let text = err.to_string();
    if text.contains("is a file not a directory") {
        Some(Some(EntryKind::File))
    } else if text.contains("http 404") || text.contains("directory not found") {
        Some(None)
    } else {
        None
    }
}

/// stat 判 `None`(目标不存在)时删除类目标的默认判型(T05 卡内边界:删除类 =
/// 按路径尾段形态):尾段含 `.` → 按文件,否则按目录。仅兜底 stat 缺失路由
/// (缺失目标在引擎侧终将如实报错,本默认只决定报错走哪条通道)。
#[must_use]
pub fn deletion_kind_fallback(remote: &str) -> EntryKind {
    let tail = remote
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(remote);
    if tail.contains('.') {
        EntryKind::File
    } else {
        EntryKind::Directory
    }
}

/// 文件类操作的引擎形状解析(T05 实锚:delete/copyfile/movefile 对**文件**的
/// 可用形状 = `{fs: 目录基座, remote: 相对名}`;bare 本地文件路径在 remote 为空
/// 时须拆 父目录/尾段名——fs 携文件路径形状实测 500 且带删除副作用,禁用)。
/// 目录类原样返回(purge 接受 fs 携路径 + `remote:""`)。
///
/// # Errors
/// bare 路径不可拆出尾段名/父目录(如 `/`、`..` 结尾)→ 结构化 Fatal。
fn entry_target_shape(
    fs: &str,
    remote: &str,
    kind: EntryKind,
) -> Result<(String, String), PartisyError> {
    if kind != EntryKind::File || !remote.is_empty() {
        return Ok((fs.to_owned(), remote.to_owned()));
    }
    let path = std::path::Path::new(fs);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            JobErrorKind::Invalid(format!(
                "file target shape: bare path has no file name: {fs:?}"
            ))
            .fatal()
        })?;
    let parent = path
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .ok_or_else(|| {
            JobErrorKind::Invalid(format!(
                "file target shape: bare path has no parent directory: {fs:?}"
            ))
            .fatal()
        })?;
    // 相对裸路径("a.txt")父目录为空 → 本地语义 "."(local 后端合法形态)。
    let parent = if parent.is_empty() {
        ".".to_owned()
    } else {
        parent
    };
    Ok((parent, name))
}

/// 判型删除路由(T05 卡内 ② D6 修复;Owner 升级裁决后定稿):文件 →
/// `operations/deletefile` **精确单文件删除**(实锚 K1/M1:多文件根/子目录两
/// 形态均只删目标,200);目录 → `operations/purge` 整树**连目录壳**删除(实锚
/// 200,含 bare 路径)。 bare 本地文件路径按实锚形状拆 父目录/尾段名。
///
/// # 路由取证与升级裁决(run 报告 §锚定取证/§升级裁决)
/// 卡面原定「文件→operations/delete」经多文件根实机裁决(2026-10-11 探针 v5)
/// 为**破坏性误删**:`operations/delete` 忽略 remote 键(T02 既有锚「remote 键
/// 无关」互证),对 fs 树做**全量文件清除**(4 文件根删 remote=a.txt → 4 文件
/// 全灭,J1);fs 携文件路径形状 = 500 且带删除副作用(B2);purge 对文件 = 干净
/// 500 零删除(N1)。实现期按作业纪律先以结构化 Fatal 安全停机过渡并升级上报,
/// Owner 裁决授权 `operations/deletefile` 入白名单(与 purge 同性质),文件路由
/// 定稿为本形态;卡面假设错误在 run 报告如实记录。
///
/// # Errors
/// 预算决策面/路径形状/severity 保真的 rc 失败均结构化上浮。
pub fn fs_remove(
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    node: &str,
    fs: &str,
    remote: &str,
    cost: u64,
    kind: EntryKind,
) -> Result<FsOpOutcome, PartisyError> {
    let (fs, remote) = entry_target_shape(fs, remote, kind)?;
    match kind {
        EntryKind::File => sync_fs_op(
            budget,
            dispatch,
            node,
            "operations/deletefile",
            &serde_json::json!({ "fs": fs, "remote": remote }),
            cost,
        ),
        EntryKind::Directory => sync_fs_op(
            budget,
            dispatch,
            node,
            "operations/purge",
            &serde_json::json!({ "fs": fs, "remote": remote }),
            cost,
        ),
    }
}

/// 源目录为空探测(T05 卡内 ① D8 提示输入):`operations/list` 单层列举,
/// `list` 数组为空/缺键 = 空(render_listing 同容错口径);非数组 = 协议损坏
/// Fatal(零猜测)。探针不设预算门(list 既有着面)。
///
/// # Errors
/// rc 失败 severity 保真上浮;`list` 非数组 → 结构化 Fatal。
fn source_dir_is_empty(
    dispatch: &impl RcDispatch,
    fs: &str,
    remote: &str,
) -> Result<bool, PartisyError> {
    let reply = dispatch.call(
        "operations/list",
        &serde_json::json!({ "fs": fs, "remote": remote }),
    )?;
    Ok(reply
        .get("list")
        .and_then(Value::as_array)
        .is_none_or(|list| list.is_empty()))
}

/// 判型传输规格([`typed_transfer`] 入参打包;src/dst 已按调用方目标解析规则
/// 拆为 fs 基座 + 基座内相对路径,remote 可空 = fs 根)。
#[derive(Debug, Clone, Copy)]
pub struct TypedTransferSpec<'a> {
    /// 预算计量 Node 键(provider profile 键;无 profile = 不限流)。
    pub node: &'a str,
    /// 预算请求成本(两通道共用:文件通道 acquire 前置/目录通道 gated_submit)。
    pub cost: u64,
    /// true = movefile/sync/move,false = copyfile/sync/copy。
    pub is_move: bool,
    /// 行内种类元数据(如 "copy"/"move";非空,JobManager 校验)。
    pub kind: &'a str,
    /// 源 fs 基座(`"remote:"` 或本地裸路径)。
    pub src_fs: &'a str,
    /// 源基座内相对路径(可空 = fs 根)。
    pub src_remote: &'a str,
    /// 目标 fs 基座。
    pub dst_fs: &'a str,
    /// 目标基座内相对路径(可空 = fs 根)。
    pub dst_remote: &'a str,
}

/// 判型传输结论(T05 卡内 ①;文件通道预算面与 [`SingleFileOutcome`] 同口径)。
/// 刻意不派生 `PartialEq`/`Clone`:载荷含 `GatedSubmit`(二者皆缺),断言面
/// 一律 `matches!`/解构(单测同口径)。
#[derive(Debug)]
pub enum TypedTransferOutcome {
    /// 单文件通道同步完成(`operations/copyfile|movefile`);载荷 = 引擎实际
    /// 落点的 dstRemote(含拷入目录时的拼名)。
    FileApplied { dst_remote: String },
    /// 单文件通道预算限流(零引擎触达,可等待重试)。
    FileThrottled { wait_until_ms: u64 },
    /// 单文件通道预算结构性拒绝(停放 queued 零引擎触达,jobs retry 可复活)。
    FileParked(JobRecord),
    /// 目录级 sync 作业(gated 全链三态);`empty_source` = 源目录为空
    /// (D8 提示输入;作业照常提交,提示由调用方渲染)。
    Sync {
        empty_source: bool,
        gated: GatedSubmit,
    },
}

/// 判型传输编排(T05 卡内 ① D5/D8 修复):stat 探源——
/// * 文件 → 单文件通道 `operations/copyfile|movefile`:dst 探型,已存在目录 →
///   拷入保持源文件名(scp 惯例),否则精确路径(尾斜杠剥除,实锚禁尾斜杠);
///   bare 本地路径文件按实锚形状拆 父目录/尾段名。
/// * 目录 → 现有目录级 sync 编排(空源照常提交 + `empty_source` 旗标,D8)。
/// * 不存在 → 卡内边界默认:创建类按目录路由(sync 作业如实上浮引擎缺失错误,
///   禁猜测吞错)。
///
/// 探针(stat/list)零预算门;操作本体 acquire 前置/gated_submit 全链。
///
/// # Errors
/// 判型失败(非锚定形态)/预算/JobManager/severity 保真 rc 失败结构化上浮。
pub fn typed_transfer(
    manager: &JobManager,
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    spec: &TypedTransferSpec<'_>,
) -> Result<TypedTransferOutcome, PartisyError> {
    match stat_kind(dispatch, spec.src_fs, spec.src_remote)? {
        Some(EntryKind::File) => {
            let (src_fs, src_remote) =
                entry_target_shape(spec.src_fs, spec.src_remote, EntryKind::File)?;
            let src_name = remote_tail_name(&src_remote)?.to_owned();
            // dst 探型失败(非锚定缺失)原样上浮:禁在「拷入目录 vs 精确路径」
            // 间猜测(精确路径可能覆盖既有文件,破坏性歧义零容忍)。
            let dst_kind = stat_kind(dispatch, spec.dst_fs, spec.dst_remote)?;
            let (dst_fs, dst_remote) = match dst_kind {
                Some(EntryKind::Directory) => {
                    let joined = if spec.dst_remote.is_empty() {
                        src_name
                    } else {
                        compose_fs(spec.dst_remote, &src_name)
                    };
                    (spec.dst_fs.to_owned(), joined)
                }
                // 缺失/文件 = scp 精确路径(尾斜杠剥除:实锚尾斜杠 500 且带副作用);
                // bare 路径根(remote 空)拆 父目录/尾段名(实锚可用形状)。
                _ => {
                    let exact = spec.dst_remote.trim_end_matches('/');
                    if exact.is_empty() {
                        let (parent, name) = entry_target_shape(spec.dst_fs, "", EntryKind::File)?;
                        (parent, name)
                    } else {
                        (spec.dst_fs.to_owned(), exact.to_owned())
                    }
                }
            };
            match budget.acquire(spec.node, spec.cost)? {
                crate::budget::Decision::Throttled { wait_until_ms } => {
                    return Ok(TypedTransferOutcome::FileThrottled { wait_until_ms });
                }
                crate::budget::Decision::Exhausted => {
                    // 结构性拒绝:停放 queued 行零触达(single_file_transfer
                    // Exhausted 面同口径,jobs retry 可复活,零静默丢弃)。
                    let parked = manager.register_queued(
                        spec.kind,
                        &compose_fs(spec.src_fs, spec.src_remote),
                        &compose_fs(spec.dst_fs, spec.dst_remote),
                    )?;
                    return Ok(TypedTransferOutcome::FileParked(parked));
                }
                crate::budget::Decision::Allow => {}
            }
            let method = if spec.is_move {
                "operations/movefile"
            } else {
                "operations/copyfile"
            };
            let params = serde_json::json!({
                "srcFs": src_fs,
                "srcRemote": src_remote,
                "dstFs": dst_fs,
                "dstRemote": dst_remote,
            });
            if let Err(err) = dispatch.call(method, &params) {
                budget.settle(spec.node, Metering::Refund(spec.cost))?;
                return Err(err);
            }
            Ok(TypedTransferOutcome::FileApplied { dst_remote })
        }
        src_kind => {
            // 目录 / 不存在(卡边界:创建类按目录路由)→ 目录级 sync 编排。
            let src_fs = compose_fs(spec.src_fs, spec.src_remote);
            let dst_fs = compose_fs(spec.dst_fs, spec.dst_remote);
            let empty_source = match src_kind {
                // 空源探测仅对确认存在的目录(缺失目标的 list 必败,缺省旗标
                // false,缺失错误由 sync 作业如实上浮,零双重报错)。
                Some(EntryKind::Directory) => {
                    source_dir_is_empty(dispatch, spec.src_fs, spec.src_remote)?
                }
                _ => false,
            };
            let gated = transfer_submit(
                manager,
                budget,
                dispatch,
                &TransferSpec {
                    node: spec.node,
                    cost: spec.cost,
                    is_move: spec.is_move,
                    kind: spec.kind,
                    src_fs: &src_fs,
                    dst_fs: &dst_fs,
                },
            )?;
            Ok(TypedTransferOutcome::Sync {
                empty_source,
                gated,
            })
        }
    }
}

/// remote 相对路径尾段名(`"a/b.txt"` → `"b.txt"`;尾斜杠容错;空 = Fatal)。
///
/// # Errors
/// 路径无尾段名(空串/全斜杠)→ 结构化 Fatal。
fn remote_tail_name(remote: &str) -> Result<&str, PartisyError> {
    let trimmed = remote.trim_end_matches('/');
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if name.is_empty() {
        return Err(
            JobErrorKind::Invalid(format!("remote path has no tail name: {remote:?}")).fatal(),
        );
    }
    Ok(name)
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

    /// Fake rc 分发(仅测试,Fake* 纪律):脚本化依序应答并记录调用(T05 起
    /// 队列元素 = Result,支持判型分类的失败脚本;纯值脚本经 `scripted` 包装)。
    struct FakeScriptDispatch {
        replies: RefCell<VecDeque<Result<Value, PartisyError>>>,
        calls: RefCell<Vec<(String, Value)>>,
    }

    impl FakeScriptDispatch {
        fn scripted(replies: &[Value]) -> Self {
            Self {
                replies: RefCell::new(replies.iter().cloned().map(Ok).collect()),
                calls: RefCell::new(Vec::new()),
            }
        }

        /// 混排脚本(T05 判型分类面:stat 可脚本化失败,锚定缺失/文件形态;
        /// PartisyError 非 Clone,按值收编)。
        fn scripted_results(replies: Vec<Result<Value, PartisyError>>) -> Self {
            Self {
                replies: RefCell::new(replies.into()),
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
                .unwrap_or_else(|| Err(JobErrorKind::Invalid("script exhausted".into()).fatal()))
        }
    }

    /// 锚定形态错误构造(stat 判型分类单测输入;文本与引擎 Display 形状同源,
    /// 2026-10-11 实锚)。
    fn anchored_err(text: &str) -> PartisyError {
        PartisyError::with_source(
            Severity::Retryable,
            Box::new(std::io::Error::other(text.to_owned())),
        )
    }

    #[test]
    fn compose_fs_and_parent_and_filter_shapes_hold() {
        // 卡内 ①:fs 拼接(remote:/本地裸路径/空段);父目录解析;过滤器实测形状。
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

    #[test]
    fn stat_kind_parses_items_and_anchors_missing_classification() {
        // T05 卡内 ①:item.IsDir 判型(实锚 2026-10-11);缺失三形态 = item:null
        // / http 404 / is a file not a directory(文件形态);其余失败原样上浮。
        let ok = FakeScriptDispatch::scripted(&[
            json!({ "item": { "Name": "tree", "IsDir": true } }),
            json!({ "item": { "Name": "a.txt", "IsDir": false } }),
            json!({ "item": null }),
            json!({}),
        ]);
        assert_eq!(
            stat_kind(&ok, "pvsrc:", "tree").expect("dir stat"),
            Some(EntryKind::Directory)
        );
        assert_eq!(
            stat_kind(&ok, "pvsrc:", "a.txt").expect("file stat"),
            Some(EntryKind::File)
        );
        assert_eq!(stat_kind(&ok, "pvsrc:", "gone").expect("item null"), None);
        assert_eq!(
            stat_kind(&ok, "pvsrc:", "gone").expect("item missing"),
            None
        );
        for (method, params) in ok.calls.borrow().iter() {
            assert_eq!(method, "operations/stat");
            let keys: Vec<&String> = params
                .as_object()
                .expect("stat params object")
                .keys()
                .collect();
            assert_eq!(keys, ["fs", "remote"], "双键形状");
        }
        // 协议损坏:IsDir 缺失/非布尔 = Fatal(零猜测)。
        let corrupt = FakeScriptDispatch::scripted(&[
            json!({ "item": { "Name": "x" } }),
            json!({ "item": { "IsDir": "yes" } }),
        ]);
        for _ in 0..2 {
            let err = stat_kind(&corrupt, "pvsrc:", "x").expect_err("corrupt item surfaces");
            assert_eq!(err.severity, Severity::Fatal);
        }
        // 锚定分类:http 404 / directory not found → None;is a file → File。
        let classified = FakeScriptDispatch::scripted_results(vec![
            Err(anchored_err(
                "rc client error (operations/stat, http 404): directory not found",
            )),
            Err(anchored_err("error in ListJSON: directory not found")),
            Err(anchored_err(
                "rc server error (operations/stat, http 500): is a file not a directory",
            )),
            Err(anchored_err("engine socket refused")),
        ]);
        assert_eq!(
            stat_kind(&classified, "/no/such", "").expect("404 classified missing"),
            None
        );
        assert_eq!(
            stat_kind(&classified, "/no/such", "").expect("text classified missing"),
            None
        );
        assert_eq!(
            stat_kind(&classified, "/a/file.txt", "").expect("file-form classified"),
            Some(EntryKind::File)
        );
        let err = stat_kind(&classified, "/x", "").expect_err("unanchored surfaces");
        assert_eq!(err.severity, Severity::Retryable, "原样保真,零吞错");
    }

    #[test]
    fn deletion_fallback_and_entry_target_shapes_hold() {
        // T05 卡内边界(删除类按尾段形态)+ 文件类形状解析(bare 路径拆
        // 父目录/尾段名;实锚:fs 携文件路径形状禁用)。
        assert_eq!(deletion_kind_fallback("a.txt"), EntryKind::File);
        assert_eq!(deletion_kind_fallback("sub/b.txt"), EntryKind::File);
        assert_eq!(deletion_kind_fallback(".hidden"), EntryKind::File);
        assert_eq!(deletion_kind_fallback("tree"), EntryKind::Directory);
        assert_eq!(deletion_kind_fallback("tree/"), EntryKind::Directory);
        assert_eq!(deletion_kind_fallback(""), EntryKind::Directory);
        assert_eq!(
            deletion_kind_fallback("dir.txt/inner"),
            EntryKind::Directory,
            "尾段形态(非全路径)"
        );
        // 目录类/具名文件:原样透传。
        assert_eq!(
            entry_target_shape("pvsrc:", "tree", EntryKind::Directory).expect("dir passthrough"),
            ("pvsrc:".to_owned(), "tree".to_owned())
        );
        assert_eq!(
            entry_target_shape("pvsrc:", "a.txt", EntryKind::File).expect("named file passthrough"),
            ("pvsrc:".to_owned(), "a.txt".to_owned())
        );
        // bare 文件路径:拆父目录/尾段名(实锚可用形状)。
        assert_eq!(
            entry_target_shape("/tmp/x/a.txt", "", EntryKind::File).expect("abs bare split"),
            ("/tmp/x".to_owned(), "a.txt".to_owned())
        );
        assert_eq!(
            entry_target_shape("a.txt", "", EntryKind::File).expect("rel bare split"),
            (".".to_owned(), "a.txt".to_owned())
        );
        let err = entry_target_shape("/", "", EntryKind::File).expect_err("root has no tail name");
        assert_eq!(err.severity, Severity::Fatal);
    }

    #[test]
    fn fs_remove_routes_deletefile_and_purge_by_kind() {
        // T05 卡内 ②(Owner 升级裁决定稿):文件 → operations/deletefile 精确
        // 单文件删除;目录 → operations/purge 整树;Throttled 零触达。
        let path =
            std::env::temp_dir().join(format!("partiverse-fsops-unit7-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let sched = budget(&path);
        let manager = JobManager::open_at(&path).expect("jobs store");
        let _ = manager;
        let ok = FakeScriptDispatch::scripted(&[json!({})]);
        assert_eq!(
            fs_remove(
                &sched,
                &ok,
                "testprov",
                "pvsrc:tree",
                "",
                1,
                EntryKind::Directory
            )
            .expect("dir purge"),
            FsOpOutcome::Applied
        );
        assert_eq!(
            ok.calls.borrow()[0].clone(),
            (
                "operations/purge".into(),
                json!({ "fs": "pvsrc:tree", "remote": "" })
            )
        );
        // bare 目录路径:双键形状(fs 携路径 + remote 空,实锚 I2/I3)。
        let bare = FakeScriptDispatch::scripted(&[json!({})]);
        fs_remove(
            &sched,
            &bare,
            "local",
            "/tmp/x/tree",
            "",
            1,
            EntryKind::Directory,
        )
        .expect("bare dir purge");
        assert_eq!(
            bare.calls.borrow()[0].clone(),
            (
                "operations/purge".into(),
                json!({ "fs": "/tmp/x/tree", "remote": "" })
            )
        );
        // 文件路由(Owner 升级裁决):operations/deletefile 精确单文件删除,
        // 双键形状(fs=基座,remote=相对路径;bare 路径拆 父目录/尾段名)。
        let file = FakeScriptDispatch::scripted(&[json!({})]);
        fs_remove(
            &sched,
            &file,
            "testprov",
            "pvsrc:",
            "a.txt",
            1,
            EntryKind::File,
        )
        .expect("file deletefile");
        assert_eq!(
            file.calls.borrow()[0].clone(),
            (
                "operations/deletefile".into(),
                json!({ "fs": "pvsrc:", "remote": "a.txt" })
            )
        );
        // bare 文件路径:拆形状后双键。
        let bare_file = FakeScriptDispatch::scripted(&[json!({})]);
        fs_remove(
            &sched,
            &bare_file,
            "local",
            "/tmp/x/a.txt",
            "",
            1,
            EntryKind::File,
        )
        .expect("bare file deletefile");
        assert_eq!(
            bare_file.calls.borrow()[0].clone(),
            (
                "operations/deletefile".into(),
                json!({ "fs": "/tmp/x", "remote": "a.txt" })
            )
        );
        // Throttled 零触达(桶容已耗尽)。
        let probe = FakeScriptDispatch::scripted(&[]);
        assert!(matches!(
            fs_remove(
                &sched,
                &probe,
                "testprov",
                "pvsrc:",
                "y.txt",
                1,
                EntryKind::File
            ),
            Ok(FsOpOutcome::Throttled { .. })
        ));
        assert_eq!(probe.calls.borrow().len(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn typed_transfer_file_channel_detects_dst_and_composes_names() {
        // T05 卡内 ① 文件通道:stat 源=文件 → dst 探型——已存在目录 → 拷入保持
        // 名;缺失/文件 → 精确路径(scp 惯例,尾斜杠剥除);bare 文件源拆形状。
        // 每子例独立预算(桶容 2,免窗口互扰)。
        let db = |tag: &str| {
            std::env::temp_dir().join(format!(
                "partiverse-fsops-unit8-{tag}-{}.db",
                std::process::id()
            ))
        };
        let manager = JobManager::open_at(&db("shared")).expect("jobs store");
        let spec = |src_remote: &'static str, dst_remote: &'static str| TypedTransferSpec {
            node: "testprov",
            cost: 1,
            is_move: false,
            kind: "copy",
            src_fs: "pvsrc:",
            src_remote,
            dst_fs: "pvdst:",
            dst_remote,
        };
        let applied_dst_remote = |outcome: TypedTransferOutcome| match outcome {
            TypedTransferOutcome::FileApplied { dst_remote } => dst_remote,
            other => panic!("expected FileApplied, got {other:?}"),
        };
        let mut dbs = vec![db("shared")];
        // dst = 已存在目录 → 拷入保持名。
        let into_dir = FakeScriptDispatch::scripted(&[
            json!({ "item": { "IsDir": false } }),
            json!({ "item": { "IsDir": true } }),
            json!({}),
        ]);
        let sched = budget(&db("into-dir"));
        dbs.push(db("into-dir"));
        let outcome = typed_transfer(&manager, &sched, &into_dir, &spec("notes.txt", "docs"))
            .expect("copy into dir");
        assert_eq!(
            applied_dst_remote(outcome),
            "docs/notes.txt",
            "拷入已存在目录 = 保持源文件名"
        );
        assert_eq!(
            into_dir.calls.borrow()[2].clone(),
            (
                "operations/copyfile".into(),
                json!({
                    "srcFs": "pvsrc:",
                    "srcRemote": "notes.txt",
                    "dstFs": "pvdst:",
                    "dstRemote": "docs/notes.txt",
                })
            )
        );
        // dst = 缺失 → 精确路径;dst = 文件 → 精确路径覆盖;尾斜杠剥除。
        for (idx, (dst_stat, dst_remote, expected)) in [
            (json!({ "item": null }), "fresh.txt", "fresh.txt"),
            (
                json!({ "item": { "IsDir": false } }),
                "exists.txt",
                "exists.txt",
            ),
            (json!({ "item": null }), "fresh.txt/", "fresh.txt"),
        ]
        .into_iter()
        .enumerate()
        {
            let exact = FakeScriptDispatch::scripted(&[
                json!({ "item": { "IsDir": false } }),
                dst_stat,
                json!({}),
            ]);
            let tag = format!("exact-{idx}");
            let sched = budget(&db(&tag));
            dbs.push(db(&tag));
            let outcome = typed_transfer(&manager, &sched, &exact, &spec("notes.txt", dst_remote))
                .expect("exact path");
            assert_eq!(applied_dst_remote(outcome), expected);
            assert_eq!(
                exact.calls.borrow()[2].clone().1.get("dstRemote"),
                Some(&json!(expected))
            );
        }
        // bare 文件源(stat 500 is a file)→ 拆形状;bare 缺失 dst → 拆形状。
        let bare = FakeScriptDispatch::scripted_results(vec![
            Err(anchored_err(
                "rc server error (operations/stat, http 500): is a file not a directory",
            )),
            Err(anchored_err(
                "rc client error (operations/stat, http 404): directory not found",
            )),
            Ok(json!({})),
        ]);
        let sched = budget(&db("bare"));
        dbs.push(db("bare"));
        let outcome = typed_transfer(
            &manager,
            &sched,
            &bare,
            &TypedTransferSpec {
                node: "local",
                cost: 1,
                is_move: false,
                kind: "copy",
                src_fs: "/tmp/x/a.pdf",
                src_remote: "",
                dst_fs: "/tmp/out/renamed.pdf",
                dst_remote: "",
            },
        )
        .expect("bare file channel");
        assert_eq!(applied_dst_remote(outcome), "renamed.pdf");
        let (method, params) = bare.calls.borrow()[2].clone();
        assert_eq!(method, "operations/copyfile");
        assert_eq!(
            params,
            json!({
                "srcFs": "/tmp/x",
                "srcRemote": "a.pdf",
                "dstFs": "/tmp/out",
                "dstRemote": "renamed.pdf",
            })
        );
        for path in dbs {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn typed_transfer_sync_route_flags_empty_source_and_defaults_missing() {
        // T05 卡内 ① 目录通道:目录 → 空源探测 + sync 编排(D8);缺失 → 按卡
        // 边界目录路由(零 list 双重报错);文件通道 Throttled 零触达。
        let db = |tag: &str| {
            std::env::temp_dir().join(format!(
                "partiverse-fsops-unit9-{tag}-{}.db",
                std::process::id()
            ))
        };
        let spec = || TypedTransferSpec {
            node: "testprov",
            cost: 1,
            is_move: false,
            kind: "copy",
            src_fs: "pvsrc:",
            src_remote: "sub",
            dst_fs: "pvdst:",
            dst_remote: "",
        };
        // ① 空目录:stat 目录 + list 空 + 提交(empty_source = true)。
        let path = db("empty");
        let _ = std::fs::remove_file(&path);
        let sched = budget(&path);
        let manager = JobManager::open_at(&path).expect("jobs store");
        let empty = FakeScriptDispatch::scripted(&[
            json!({ "item": { "IsDir": true } }),
            json!({ "list": [] }),
            json!({ "jobid": 21 }),
        ]);
        let outcome = typed_transfer(&manager, &sched, &empty, &spec()).expect("empty dir sync");
        match &outcome {
            TypedTransferOutcome::Sync {
                empty_source,
                gated,
            } => {
                assert!(*empty_source, "空源旗标必真: {outcome:?}");
                let GatedSubmit::Submitted(record) = gated else {
                    panic!("expected Submitted, got {gated:?}")
                };
                assert_eq!(record.engine_job_id, Some(21));
            }
            other => panic!("expected Sync, got {other:?}"),
        }
        let (method, params) = empty.calls.borrow()[2].clone();
        assert_eq!(method, "sync/copy");
        assert_eq!(
            params,
            json!({ "srcFs": "pvsrc:sub", "dstFs": "pvdst:", "_async": true })
        );
        // ② 非空目录:空源旗标 false(独立预算,免窗口互扰)。
        let path = db("full");
        let _ = std::fs::remove_file(&path);
        let sched = budget(&path);
        let manager = JobManager::open_at(&path).expect("jobs store");
        let full = FakeScriptDispatch::scripted(&[
            json!({ "item": { "IsDir": true } }),
            json!({ "list": [{ "Name": "a.txt", "IsDir": false }] }),
            json!({ "jobid": 22 }),
        ]);
        let outcome = typed_transfer(&manager, &sched, &full, &spec()).expect("full dir sync");
        assert!(matches!(
            outcome,
            TypedTransferOutcome::Sync {
                empty_source: false,
                ..
            }
        ));
        // ③ 缺失源:零 list(不做双重报错),直接 sync 路由。
        let path = db("missing");
        let _ = std::fs::remove_file(&path);
        let sched = budget(&path);
        let manager = JobManager::open_at(&path).expect("jobs store");
        let missing =
            FakeScriptDispatch::scripted(&[json!({ "item": null }), json!({ "jobid": 23 })]);
        let outcome = typed_transfer(&manager, &sched, &missing, &spec()).expect("missing sync");
        assert!(matches!(
            outcome,
            TypedTransferOutcome::Sync {
                empty_source: false,
                ..
            }
        ));
        assert_eq!(missing.calls.borrow().len(), 2, "缺失源零 list 触达");
        // ④ 文件通道 Throttled:预扣 1/2 后 cost=2 → FileThrottled 零触达。
        let path = db("throttled");
        let _ = std::fs::remove_file(&path);
        let sched = budget(&path);
        let manager = JobManager::open_at(&path).expect("jobs store");
        assert_eq!(
            sched.acquire("testprov", 1).expect("pre-charge"),
            Decision::Allow
        );
        let depleted = FakeScriptDispatch::scripted(&[
            json!({ "item": { "IsDir": false } }),
            json!({ "item": { "IsDir": true } }),
        ]);
        let outcome = typed_transfer(
            &manager,
            &sched,
            &depleted,
            &TypedTransferSpec { cost: 2, ..spec() },
        )
        .expect("file throttled");
        assert!(matches!(
            outcome,
            TypedTransferOutcome::FileThrottled { .. }
        ));
        assert_eq!(depleted.calls.borrow().len(), 2, "预算门前仅两次 stat");
        for path in ["empty", "full", "missing", "throttled"] {
            let _ = std::fs::remove_file(db(path));
        }
    }

    #[test]
    fn remote_tail_name_extracts_and_rejects_empty() {
        // 尾段名解析:嵌套/单段/尾斜杠;空段 = Fatal。
        assert_eq!(remote_tail_name("a/b.txt").expect("nested"), "b.txt");
        assert_eq!(remote_tail_name("b.txt").expect("flat"), "b.txt");
        assert_eq!(remote_tail_name("dir/").expect("trailing slash"), "dir");
        let err = remote_tail_name("").expect_err("empty rejected");
        assert_eq!(err.severity, Severity::Fatal);
        assert!(remote_tail_name("///").is_err());
    }
}
