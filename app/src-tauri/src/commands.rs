//! IPC 命令面(M1-WP05-T01 数据接线)。
//! 壳薄纪律:命令只做参数透传与错误映射,业务逻辑一律在 crates;crates 同步
//! API 按 ADR-0004 口径经 `run_blocking`(spawn_blocking)包装;错误统一
//! `CmdError`(`{kind,msg,severity}`)上浮,零吞错。

use std::sync::Arc;

use partiverse_core::budget::JobSpec;
use partiverse_core::jobs::RcDispatch;
use partiverse_engine::slots::{EngineSlotConfig, DEFAULT_SLOT_ID};
use serde_json::Value;
// `package_info` 在 Tauri 2 里是 AppHandle 的固有方法,无需 Manager trait(cargo check 实证)。
use tauri::{AppHandle, State, Wry};

use crate::dto::{
    BudgetDecision, EngineSnapshot, GatedSubmitOut, JobPollOut, JobRecordOut, ProviderFormOut,
};
use crate::error::{CmdError, ErrorKind, Severity};
use crate::state::{lock, ShellState};

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

/// 轮询单个 job(job/status 观测推进状态机;终态幂等不再触引擎)。
#[tauri::command]
#[specta::specta]
pub async fn job_poll(state: State<'_, ShellState>, id: String) -> Result<JobPollOut, CmdError> {
    let jobs = Arc::clone(&state.jobs);
    let dispatch = state.dispatch()?;
    run_blocking(move || {
        jobs.poll(&dispatch, &id)
            .map_err(CmdError::from)
            .map(JobPollOut::from)
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
