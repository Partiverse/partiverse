//! IPC 命令面(M1-WP05-T01 数据接线)。
//! 壳薄纪律:命令只做参数透传与错误映射,业务逻辑一律在 crates;crates 同步
//! API 按 ADR-0004 口径经 `run_blocking`(spawn_blocking)包装;错误统一
//! `CmdError`(`{kind,msg,severity}`)上浮,零吞错。

use std::sync::Arc;

use partiverse_core::budget::{Decision, GatedSubmit, JobSpec};
use partiverse_core::jobs::{JobStatus, RcDispatch};
use partiverse_core::oauth::{BaiduOAuthFlow, ClientCredentials, OAuthTokenSink};
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

/// 浏览数据源:rc 白名单 `operations/list` 单目录列举(fs = "remote:path");
/// 载荷 JSON 文本原样透传(壳零字段裁剪、零 schema 发明;理由见 dto.rs)。
#[tauri::command]
#[specta::specta]
pub async fn operations_list(state: State<'_, ShellState>, fs: String) -> Result<String, CmdError> {
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        dispatch
            .call("operations/list", &serde_json::json!({ "fs": fs }))
            .map_err(CmdError::from)
            .map(|reply| reply.to_string())
    })
    .await
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
