//! IPC 命令面(M1-WP05-T01 数据接线)。
//! 壳薄纪律:命令只做参数透传与错误映射,业务逻辑一律在 crates;crates 同步
//! API 按 ADR-0004 口径经 `run_blocking`(spawn_blocking)包装;错误统一
//! `CmdError`(`{kind,msg,severity}`)上浮,零吞错。

use std::sync::Arc;

use partiverse_core::budget::{Decision, GatedSubmit, JobSpec};
use partiverse_core::jobs::{JobStatus, RcDispatch};
use partiverse_core::oauth::{BaiduOAuthFlow, ClientCredentials, OAuthTokenSink};
use partiverse_core::schema::{create_remote, RemoteCreate};
use partiverse_engine::slots::{EngineSlotConfig, DEFAULT_SLOT_ID};
use serde_json::Value;
// `package_info` 在 Tauri 2 里是 AppHandle 的固有方法,无需 Manager trait(cargo check 实证)。
use tauri::{AppHandle, State, Wry};

use crate::dto::{
    BudgetDecision, EngineSnapshot, FsOpOutcomeOut, GatedSubmitOut, JobPollOut, JobRecordOut,
    JobRetryOut, JobStatusOut, ProviderFormOut,
};
use crate::error::{CmdError, ErrorKind, Severity};
use crate::state::{lock, InMemoryTokenSink, ShellState};

/// 同步阻塞任务包装(ADR-0004 口径);join 失败(任务 panic)= Fatal 上浮。
async fn run_blocking<T, F>(task: F) -> Result<T, CmdError>
where
    F: FnOnce() -> Result<T, CmdError> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|err| {
            CmdError::new(
                ErrorKind::Internal,
                Severity::Fatal,
                format!("blocking task join failed: {err}"),
            )
        })?
}

/// 示范命令:返回应用版本号(验证 specta 强类型 IPC 管道与错误通道)。
#[tauri::command]
#[specta::specta]
pub fn app_version(app: AppHandle<Wry>) -> Result<String, CmdError> {
    Ok(app.package_info().version.to_string())
}

/// 确保引擎槽位就绪(幂等;冷路径含安装校验+rcd spawn+探活);slot_id 缺省
/// = default 槽位;成功后句柄快照入壳状态并返回。
#[tauri::command]
#[specta::specta]
pub async fn engine_ensure(
    state: State<'_, ShellState>,
    slot_id: Option<String>,
) -> Result<EngineSnapshot, CmdError> {
    let coordinator = Arc::clone(&state.coordinator);
    let engine = Arc::clone(&state.engine);
    run_blocking(move || {
        let id = slot_id.unwrap_or_else(|| DEFAULT_SLOT_ID.to_owned());
        let slot = EngineSlotConfig::load_default(&id)?;
        let handle = lock(&coordinator)?.ensure(&slot)?;
        let snapshot = EngineSnapshot::from(&handle);
        *lock(&engine)? = Some(handle);
        Ok(snapshot)
    })
    .await
}

/// 引擎状态快照(ensure 时刻观测值,零引擎触达;未拉起 → null)。
#[tauri::command]
#[specta::specta]
pub async fn engine_status(
    state: State<'_, ShellState>,
) -> Result<Option<EngineSnapshot>, CmdError> {
    let guard = lock(&state.engine)?;
    Ok(guard.as_ref().map(EngineSnapshot::from))
}

/// 优雅退出引擎槽位(slot_id 缺省 = default;成功后清除壳内句柄;
/// id 不匹配/无活动槽位 = Fatal,句柄不动)。
#[tauri::command]
#[specta::specta]
pub async fn engine_shutdown(
    state: State<'_, ShellState>,
    slot_id: Option<String>,
) -> Result<(), CmdError> {
    let coordinator = Arc::clone(&state.coordinator);
    let engine = Arc::clone(&state.engine);
    run_blocking(move || {
        let id = slot_id.unwrap_or_else(|| DEFAULT_SLOT_ID.to_owned());
        lock(&coordinator)?.shutdown(&id)?;
        *lock(&engine)? = None;
        Ok(())
    })
    .await
}

/// 拉取全部 provider 连接表单 schema(经 rc 白名单 `config/providers`)。
#[tauri::command]
#[specta::specta]
pub async fn providers_fetch(
    state: State<'_, ShellState>,
) -> Result<Vec<ProviderFormOut>, CmdError> {
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        let forms = partiverse_core::schema::provider_forms(&dispatch)?;
        Ok(forms.iter().map(ProviderFormOut::from).collect())
    })
    .await
}

/// 123 云盘(官方 WebDAV)接入:表单值构造 remote 并经 `config/create` 注入
/// (端点用户粘贴零硬编码;密码经 rclone obscure 落加密 config)。
#[tauri::command]
#[specta::specta]
pub async fn connection_create_123(
    state: State<'_, ShellState>,
    name: String,
    endpoint: String,
    account: String,
    app_password: String,
) -> Result<(), CmdError> {
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        let remote =
            partiverse_core::pan123::webdav_remote(&name, &endpoint, &account, &app_password)?;
        partiverse_core::schema::create_remote(&dispatch, &remote)?;
        Ok(())
    })
    .await
}

/// 百度 oob 式本地换码(M1-WP05-T08,R2 凭据钉子):code/state + client 凭据
/// 全部由前端注入(用户侧自建应用,禁硬编码/禁日志),经 core
/// `BaiduOAuthFlow::exchange_code` 在本机 GET 官方 token 端点换码(code 换
/// token 必须本地完成,凭据零过服务器);token 经 `OAuthTokenSink` 口径交壳面
/// 过渡件 [`InMemoryTokenSink`]——仅保留进程内存,零落盘零日志,零 token 数据
/// 回线(返回值恒 `()`)。加密 config 最终落点沿 T02 口径延后:引擎钉定
/// rclone v1.75.1 无 baidu backend(实测 `config/create` 500),待 backends-go
/// 百度后端落地后另立卡接线;沙箱提示与向导接线同期(向导守卫暂保持
/// channelUnavailable 上浮,Owner 裁定 2026-10-10)。
#[tauri::command]
#[specta::specta]
pub async fn baidu_exchange_code(
    state: State<'_, ShellState>,
    client_id: String,
    client_secret: String,
    code: String,
    oauth_state: String,
) -> Result<(), CmdError> {
    let slot = Arc::clone(&state.token_slot);
    run_blocking(move || {
        // 凭据非空校验在 core(空白 → ClientCredentialsMissing 引导错误,Fatal);
        // state 为 oob 回填路径的会话比对值(core 校验非空,不入 token 请求)。
        let credentials = ClientCredentials::new(client_id, client_secret)?;
        let token = BaiduOAuthFlow::new(credentials).exchange_code(&code, &oauth_state)?;
        InMemoryTokenSink::new(slot).store_token(&token)?;
        Ok(())
    })
    .await
}

/// 协议类通道创建(M1-WP05-T09 DoD①,修复 F1「Create channel not available」):
/// 前端表单值以 JSON 文本过线(T01 specta 口径,动态 JSON 一律 String),经
/// ShellDispatch 透传 rc `config/create`(白名单内;core `create_remote` 复用,
/// core 零改动)。成功后立即 `config/get` 回读断言(DoD④「真建 remote→回读」
/// 在壳单点收口,向导与集成测试同消费面)。
///
/// # rc 形状实机锚定(2026-10-11,宿主件 = 引擎钉定 rclone v1.75.1,禁凭记忆)
/// - `config/create` 入参 `{"name","type","parameters","obscure"}`:local 后端
///   `parameters:{}` → 200 `{}`;alias 后端 `parameters:{"remote":"<目标>"}` →
///   200 `{}`;同名重复创建 = 静默覆盖(200);未知 backend → 500(与 T03/T08
///   实测记录一致)。`obscure:true` 显式化(core `create_remote` 已固化)。
/// - `config/get` 入参 `{"name"}` → 回读扁平 remote 配置,恒含 `type` 键
///   (local 实测回 `{"type":"local"}`;alias 回 `{"remote":…,"type":"alias"}`)。
///
/// # 凭据过壳纪律(R2 口径)
/// parameters 文本可含密码明文:解析失败错误只含 serde 根因零原文回显;core
/// `create_remote` 错误路径已对全部参数值 `redact` 兜底;壳全程零日志,Debug
/// 面(`RemoteCreate` 手写)只出键名零值。
///
/// # Errors
/// parameters 非 JSON 对象 → Internal Fatal(零原文回显);rc 失败 → Core
/// (severity 保真,值已脱敏);回读 `type` 不符 → Core Fatal(结构化,零参数值)。
#[tauri::command]
#[specta::specta]
pub async fn connection_create_protocol(
    state: State<'_, ShellState>,
    remote_name: String,
    backend_type: String,
    parameters: String,
) -> Result<(), CmdError> {
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        let parsed: Value = serde_json::from_str(&parameters).map_err(|err| {
            // 错误消息只含 serde 解析根因,零原文回显(parameters 可含密码明文)。
            CmdError::new(
                ErrorKind::Internal,
                Severity::Fatal,
                format!("connection_create_protocol: parameters must be a JSON object: {err}"),
            )
        })?;
        connection_create_protocol_inner(&dispatch, &remote_name, &backend_type, &parsed)
    })
    .await
}

/// [`connection_create_protocol`] 的同步体(向导与真引擎测试同消费面):
/// parameters JSON 对象 → `RemoteCreate`(core `create_remote` 白名单透传)
/// → `config/get` 回读断言 `type` 一致(回包扁平配置恒含 `type`,实测锚定)。
///
/// # Errors
/// 见 [`connection_create_protocol`]。
pub(crate) fn connection_create_protocol_inner(
    dispatch: &impl RcDispatch,
    remote_name: &str,
    backend_type: &str,
    parameters: &Value,
) -> Result<(), CmdError> {
    let object = parameters.as_object().ok_or_else(|| {
        CmdError::new(
            ErrorKind::Internal,
            Severity::Fatal,
            "connection_create_protocol: parameters must be a JSON object",
        )
    })?;
    let mut remote = RemoteCreate::new(remote_name, backend_type);
    for (key, value) in object {
        remote = remote.with_parameter(key.clone(), value.clone());
    }
    create_remote(dispatch, &remote).map_err(CmdError::from)?;
    let reply = dispatch
        .call("config/get", &serde_json::json!({ "name": remote_name }))
        .map_err(CmdError::from)?;
    let readback = reply.get("type").and_then(Value::as_str);
    if readback != Some(backend_type) {
        // 回读断言失败:只报事实键值(非凭据),零参数值入载荷。
        return Err(CmdError::new(
            ErrorKind::Core,
            Severity::Fatal,
            format!(
                "connection_create_protocol: config/get readback type mismatch for remote \
                 `{remote_name}`: expected `{backend_type}`, got `{}`",
                readback.unwrap_or("<missing>")
            ),
        ));
    }
    Ok(())
}

/// 用户家目录(M1-WP05-T09 DoD③,修复 F3 浏览默认根=/):unix 解析 `HOME`、
/// windows 解析 `USERPROFILE`,零硬编码路径;缺失/空值 = Config Fatal 上浮,
/// 禁静默回退(回退 = 把权限问题藏成 UX 缺陷)。前端消费点 = 浏览默认根。
#[tauri::command]
#[specta::specta]
pub fn user_home_dir() -> Result<String, CmdError> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    // ErrorKind::Config 首个生产构造点(此前仅有测试构造,expect 属性已撤)。
    std::env::var(key)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            CmdError::new(
                ErrorKind::Config,
                Severity::Fatal,
                format!("user home directory env var `{key}` is missing or empty"),
            )
        })
}

/// 浏览数据源:rc 白名单 `operations/list` 单目录列举(fs = "remote:path";
/// 载荷 JSON 文本原样透传(壳零字段裁剪、零 schema 发明;理由见 dto.rs)。
///
/// # rc 形状实机锚定(2026-10-11,rclone v1.75.1,修复 F2 浏览空白)
/// `operations/list` 要求 `fs` 与 `remote` 两键齐备:仅 `fs` → 400 `Didn't
/// find key "remote" in input`(T01 形状记忆缺口的实锤);`remote:""` 合法
/// (列举 fs 根);裸路径 fs(local 后端)+ `remote:""` → 200,`operations/
/// mkdir` 同形状实测落盘成功。T09 起两键恒齐传。
#[tauri::command]
#[specta::specta]
pub async fn operations_list(
    state: State<'_, ShellState>,
    fs: String,
    remote: String,
) -> Result<String, CmdError> {
    let dispatch = state.dispatch()?;
    run_blocking(move || operations_list_inner(&dispatch, &fs, &remote)).await
}

/// [`operations_list`] 的同步体(真引擎测试同消费面;两键齐传,remote 空串合法)。
///
/// # Errors
/// rc 失败按引擎 severity 保真映射 Core 上浮(零吞错)。
pub(crate) fn operations_list_inner(
    dispatch: &impl RcDispatch,
    fs: &str,
    remote: &str,
) -> Result<String, CmdError> {
    dispatch
        .call(
            "operations/list",
            &serde_json::json!({ "fs": fs, "remote": remote }),
        )
        .map_err(CmdError::from)
        .map(|reply| reply.to_string())
}

/// 预算决策查询(core 预算器 acquire,供前端提交前预估等待;cost 取 u32——
/// specta-typescript 禁导 64 位整型,调用点无损加宽至 core 的 u64)。
#[tauri::command]
#[specta::specta]
pub async fn budget_acquire(
    state: State<'_, ShellState>,
    node: String,
    cost: u32,
) -> Result<BudgetDecision, CmdError> {
    let budget = Arc::clone(&state.budget);
    run_blocking(move || {
        budget
            .acquire(&node, u64::from(cost))
            .map_err(CmdError::from)
            .map(BudgetDecision::from)
    })
    .await
}

/// 提交异步 rc job(预算前置门:core `gated_submit` 先 acquire 后 submit,
/// Exhausted 时 job 留 queued 零引擎触达——编排层强制,前端不可绕过;
/// params = JSON 对象文本,壳显式解析,解析失败 Fatal 上浮)。
#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
pub async fn job_submit(
    state: State<'_, ShellState>,
    node: String,
    method: String,
    kind: String,
    src: String,
    dst: String,
    params: String,
    cost: u32,
) -> Result<GatedSubmitOut, CmdError> {
    let budget = Arc::clone(&state.budget);
    let jobs = Arc::clone(&state.jobs);
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        let params: Value = serde_json::from_str(&params).map_err(|err| {
            CmdError::new(
                ErrorKind::Internal,
                Severity::Fatal,
                format!("job_submit: params must be valid JSON text: {err}"),
            )
        })?;
        let spec = JobSpec {
            method: &method,
            kind: &kind,
            src: &src,
            dst: &dst,
            params: &params,
        };
        budget
            .gated_submit(&jobs, &dispatch, &node, u64::from(cost), spec)
            .map_err(CmdError::from)
            .map(GatedSubmitOut::from)
    })
    .await
}

/// 轮询单个 job(M1-WP06-T02 升级:core `poll_with_progress`——job/status 推进
/// 状态机 + running 带锚行顺带 core/stats 组采样落进度列;终态/停放行零引擎
/// 触达,采样缺失不落列,采样失败结构化上浮)。
#[tauri::command]
#[specta::specta]
pub async fn job_poll(state: State<'_, ShellState>, id: String) -> Result<JobPollOut, CmdError> {
    let jobs = Arc::clone(&state.jobs);
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        partiverse_core::fsops::poll_with_progress(&jobs, &dispatch, &id)
            .map_err(CmdError::from)
            .map(JobPollOut::from)
    })
    .await
}

/// 公开任务清单(M1-WP06-T02 卡内 ③):status 过滤(None = 全量),按
/// created_at, id 稳定序;传输队列面板单源。
#[tauri::command]
#[specta::specta]
pub async fn job_list(
    state: State<'_, ShellState>,
    status: Option<JobStatusOut>,
) -> Result<Vec<JobRecordOut>, CmdError> {
    let jobs = Arc::clone(&state.jobs);
    run_blocking(move || {
        let records = jobs.list_jobs(status.map(JobStatusOut::to_core))?;
        Ok(records.into_iter().map(JobRecordOut::from).collect())
    })
    .await
}

/// 取消 job(core `job_cancel`:带锚行 job/stop + 终态裁决 error/user_canceled/
/// Interrupted;终态行取消 = 非法迁移 Fatal)。
#[tauri::command]
#[specta::specta]
pub async fn job_cancel(
    state: State<'_, ShellState>,
    id: String,
) -> Result<JobRecordOut, CmdError> {
    let jobs = Arc::clone(&state.jobs);
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        jobs.job_cancel(&dispatch, &id)
            .map_err(CmdError::from)
            .map(JobRecordOut::from)
    })
    .await
}

/// 重试 job(M1-WP06-T02 卡内 ③;编排层重试退避/re-acquire 执行点):
/// 停放行(queued 无锚,预算 Exhausted/自动回队/提交窗残留)→ 预算 re-acquire
/// 前置,Allow 即 `revive_parked` 原地复活(同 id,清上次失败标注);error 终态
/// 行(重试超限/Fatal/用户取消,终态零出边)→ 预算门提交**新** job(元数据由
/// 调用方照首次提交原样供给,行内不落 params,红线 3;旧行留痕);其余状态
/// (running/done/带锚 queued)拒绝 Fatal,零出边。
#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
pub async fn job_retry(
    state: State<'_, ShellState>,
    id: String,
    node: String,
    method: String,
    kind: String,
    src: String,
    dst: String,
    params: String,
    cost: u32,
) -> Result<JobRetryOut, CmdError> {
    let budget = Arc::clone(&state.budget);
    let jobs = Arc::clone(&state.jobs);
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        let params: Value = serde_json::from_str(&params).map_err(|err| {
            CmdError::new(
                ErrorKind::Internal,
                Severity::Fatal,
                format!("job_retry: params must be valid JSON text: {err}"),
            )
        })?;
        let record = jobs.get(&id).map_err(CmdError::from)?;
        let cost = u64::from(cost);
        match record.status {
            JobStatus::Queued if record.engine_job_id.is_none() => {
                match budget.acquire(&node, cost).map_err(CmdError::from)? {
                    Decision::Allow => Ok(JobRetryOut::Revived(JobRecordOut::from(
                        jobs.revive_parked(&dispatch, &id, &method, &params)
                            .map_err(CmdError::from)?,
                    ))),
                    Decision::Throttled { wait_until_ms } => {
                        Ok(JobRetryOut::Throttled { wait_until_ms })
                    }
                    Decision::Exhausted => Ok(JobRetryOut::Exhausted(JobRecordOut::from(
                        jobs.get(&id).map_err(CmdError::from)?,
                    ))),
                }
            }
            JobStatus::Error => {
                let spec = JobSpec {
                    method: &method,
                    kind: &kind,
                    src: &src,
                    dst: &dst,
                    params: &params,
                };
                match budget
                    .gated_submit(&jobs, &dispatch, &node, cost, spec)
                    .map_err(CmdError::from)?
                {
                    GatedSubmit::Submitted(record) => {
                        Ok(JobRetryOut::Submitted(JobRecordOut::from(record)))
                    }
                    GatedSubmit::Throttled { wait_until_ms } => {
                        Ok(JobRetryOut::Throttled { wait_until_ms })
                    }
                    GatedSubmit::Exhausted(record) => {
                        Ok(JobRetryOut::Exhausted(JobRecordOut::from(record)))
                    }
                }
            }
            other => Err(CmdError::new(
                ErrorKind::Core,
                Severity::Fatal,
                format!("job `{id}` is not retryable in status `{}`", other.as_str()),
            )),
        }
    })
    .await
}

/// 新建目录(M1-WP06-T02 卡内 ③):core `fsops::fs_mkdir`(预算 acquire 前置,
/// rc `operations/mkdir` 双参数实测形状);Throttled/Exhausted 零引擎触达原样
/// 上浮,由前端择时重试。
#[tauri::command]
#[specta::specta]
pub async fn fs_mkdir(
    state: State<'_, ShellState>,
    node: String,
    fs: String,
    remote: String,
    cost: u32,
) -> Result<FsOpOutcomeOut, CmdError> {
    let budget = Arc::clone(&state.budget);
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        partiverse_core::fsops::fs_mkdir(&budget, &dispatch, &node, &fs, &remote, u64::from(cost))
            .map_err(CmdError::from)
            .map(FsOpOutcomeOut::from)
    })
    .await
}

/// 删除目录树文件(M1-WP06-T02 卡内 ③):core `fsops::fs_delete`(rc
/// `operations/delete`,实测语义 = 递归删除该目录树下全部文件、保留目录壳;
/// 指向文件 = 引擎 500 原样上浮)。前端必须预览-提交(破坏性操作禁直通)。
#[tauri::command]
#[specta::specta]
pub async fn fs_delete(
    state: State<'_, ShellState>,
    node: String,
    fs: String,
    cost: u32,
) -> Result<FsOpOutcomeOut, CmdError> {
    let budget = Arc::clone(&state.budget);
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        partiverse_core::fsops::fs_delete(&budget, &dispatch, &node, &fs, u64::from(cost))
            .map_err(CmdError::from)
            .map(FsOpOutcomeOut::from)
    })
    .await
}

/// 启动对账(running 且引擎侧无锚 → error 终态;返回本次对账改判清单)。
#[tauri::command]
#[specta::specta]
pub async fn jobs_reconcile(state: State<'_, ShellState>) -> Result<Vec<JobRecordOut>, CmdError> {
    let jobs = Arc::clone(&state.jobs);
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        let records = jobs.reconcile(&dispatch)?;
        Ok(records.into_iter().map(JobRecordOut::from).collect())
    })
    .await
}

/// T09 真引擎测试(unix 门;真 rclone 1.75.1 离线,零真实网络:后端仅 local/
/// alias,config 只存不连)。纪律:与 core 集成测试同款——预装引擎预检防误触
/// 下载、进程内串行(安装根共享)、测试 remote `config/delete` 清场零残留。
#[cfg(all(test, unix))]
mod real_engine_tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    use partiverse_core::error::PartisyError;
    use partiverse_engine::{
        EngineCoordinator, EngineInstaller, EngineManifest, EngineSlotConfig, DEFAULT_SLOT_ID,
    };
    use serde_json::json;

    use super::*;
    use crate::state::ShellDispatch;

    /// 集成测试进程内串行(引擎安装根共享路径,core 集成同款纪律)。
    fn test_serial_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// 预装引擎预检(防误触网络下载;缺失 → 显式失败+安装指引)。
    fn require_preinstalled_engine() -> PathBuf {
        let root = EngineInstaller::resolve_install_root().expect("engine install root resolvable");
        let manifest = EngineManifest::embedded().expect("embedded manifest parses");
        let asset = manifest.current_platform().expect("host platform asset");
        let binary = root.join(&manifest.version).join(&asset.binary_name);
        assert!(
            binary.is_file(),
            "rclone missing at {}; run engine-install",
            root.display()
        );
        binary
    }

    /// 真引擎沙箱:独立临时工作区 + 槽位缓存隔离 + ShellDispatch 测试构造面。
    struct TestEngine {
        /// 持有协调器(rcd 子进程随 Drop 生命周期管理,测试期必须存活)。
        _coordinator: EngineCoordinator,
        dispatch: ShellDispatch,
        work: PathBuf,
    }

    fn spawn_engine(tag: &str) -> TestEngine {
        // 预装预检(缺失即失败,防协调器误触网络下载);二进制定位由协调器按
        // 槽位 engine_root 自理,此处仅校验存在性。
        require_preinstalled_engine();
        let work =
            std::env::temp_dir().join(format!("partiverse-shell-t09-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work); // 上轮残留清理(测试沙箱路径,非生产)
        let mut slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("default slot loads");
        slot.cache_dir = work.join("cache");
        fs::create_dir_all(&slot.cache_dir).expect("create test cache dir");
        let mut coordinator = EngineCoordinator::new().expect("coordinator constructs");
        let handle = coordinator.ensure(&slot).expect("ensure engine");
        TestEngine {
            _coordinator: coordinator,
            dispatch: ShellDispatch::for_tests(handle.rc_client.clone()),
            work,
        }
    }

    /// 收场清场:测试 remote `config/delete`(白名单内)零残留。
    fn cleanup_remotes(dispatch: &ShellDispatch, names: &[&str]) {
        for name in names {
            dispatch
                .call("config/delete", &json!({ "name": name }))
                .expect("cleanup config/delete");
        }
    }

    /// 拒触引擎替身:非对象参数必须在触达 rc 前结构化拒绝(命中 call 即败)。
    struct FakeNoCallDispatch;

    impl RcDispatch for FakeNoCallDispatch {
        fn call(&self, method: &str, _params: &Value) -> Result<Value, PartisyError> {
            panic!("dispatch must not be touched, but got {method}");
        }
    }

    #[test]
    fn connection_create_protocol_local_and_alias_readback_on_real_engine() {
        let _serial = test_serial_lock().lock().expect("serial lock");
        let engine = spawn_engine("create");
        // local 后端:parameters 空对象(实测最小形状),inner 自带 config/get
        // 回读 type 断言(失败即 Err)。
        connection_create_protocol_inner(&engine.dispatch, "pv-shell-local", "local", &json!({}))
            .expect("local create + readback ok");
        // alias 后端:parameters.remote 指向本地临时目录(实测键名 = remote)。
        let target = engine.work.to_string_lossy().to_string();
        connection_create_protocol_inner(
            &engine.dispatch,
            "pv-shell-alias",
            "alias",
            &json!({ "remote": target }),
        )
        .expect("alias create + readback ok");
        // DoD「config/get 回读」显式面:扁平配置恒含 type,与创建类型一致。
        for (name, expected) in [("pv-shell-local", "local"), ("pv-shell-alias", "alias")] {
            let reply = engine
                .dispatch
                .call("config/get", &json!({ "name": name }))
                .expect("config/get ok");
            assert_eq!(reply.get("type").and_then(Value::as_str), Some(expected));
        }
        cleanup_remotes(&engine.dispatch, &["pv-shell-local", "pv-shell-alias"]);
    }

    #[test]
    fn operations_list_requires_and_uses_remote_key_on_real_engine() {
        let _serial = test_serial_lock().lock().expect("serial lock");
        let engine = spawn_engine("list");
        fs::write(engine.work.join("seed.txt"), "seed").expect("seed file");
        let target = engine.work.to_string_lossy().to_string();
        connection_create_protocol_inner(
            &engine.dispatch,
            "pv-shell-list",
            "alias",
            &json!({ "remote": target }),
        )
        .expect("alias create + readback ok");
        // F2 根因锚定:仅 fs 单键(修复前形状)→ 引擎 400 拒绝,错误文本点名
        // remote 键缺失(T01 形状记忆缺口的实机实锤)。
        let err = engine
            .dispatch
            .call("operations/list", &json!({ "fs": "pv-shell-list:" }))
            .expect_err("fs-only shape must be rejected by engine");
        assert!(
            err.to_string().contains("remote"),
            "engine error must name the missing key: {err}"
        );
        // 修复后形状(两键齐传,remote 空串合法)→ 列举成功且回读见种子文件。
        let payload =
            operations_list_inner(&engine.dispatch, "pv-shell-list:", "").expect("list ok");
        let parsed: Value = serde_json::from_str(&payload).expect("payload is JSON text");
        let names: Vec<&str> = parsed["list"]
            .as_array()
            .expect("list array")
            .iter()
            .filter_map(|entry| entry.get("Name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"seed.txt"), "names: {names:?}");
        cleanup_remotes(&engine.dispatch, &["pv-shell-list"]);
    }

    #[test]
    fn create_error_surface_carries_no_parameter_values() {
        let _serial = test_serial_lock().lock().expect("serial lock");
        let engine = spawn_engine("redact");
        // 未知 backend → 引擎 500(core create_remote 拒绝载荷值级 redact 兜底):
        // 壳错误面零密码明文(凭据过壳纪律),非凭据根因(backend 类型)保留。
        let err = connection_create_protocol_inner(
            &engine.dispatch,
            "pv-shell-redact",
            "no-such-backend",
            &json!({ "pass": "s3cr3t-pass-value" }),
        )
        .expect_err("unknown backend must be rejected");
        assert!(
            !err.msg.contains("s3cr3t-pass-value"),
            "credential leaked into error surface: {}",
            err.msg
        );
        assert!(
            err.msg.contains("no-such-backend"),
            "root cause lost: {}",
            err.msg
        );
        // 未建成 remote(500 拒绝路径),零残留可清。
    }

    #[test]
    fn non_object_parameters_are_structured_fatal_without_engine_touch() {
        let err = connection_create_protocol_inner(
            &FakeNoCallDispatch,
            "pv-shell-bad",
            "local",
            &json!("not-an-object"),
        )
        .expect_err("non-object parameters must be rejected");
        assert_eq!(err.kind, ErrorKind::Internal);
        assert_eq!(err.severity, Severity::Fatal);
        assert!(
            !err.msg.contains("not-an-object"),
            "raw payload echoed: {}",
            err.msg
        );
    }

    #[test]
    fn user_home_dir_resolves_env_without_hardcoding() {
        let home = user_home_dir().expect("home resolvable in test env");
        let expected = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .expect("home env var present");
        assert_eq!(home, expected, "must resolve env verbatim, zero hardcoding");
        assert!(!home.is_empty());
    }
}
