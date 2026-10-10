//! 壳共享状态与 rc 分发适配(M1-WP05-T01)。
//! 壳薄纪律:状态仅聚合 crates 已有件,零业务逻辑;`ShellDispatch` 是 engine
//! `RcClient` → core `RcDispatch` 唯一接缝适配(白名单收口+severity 保真,
//! 形态对齐 core 集成测试 `RcAdapter`),错误全部映射 `CmdError` 上浮。

use std::sync::{Arc, Mutex, MutexGuard};

use partiverse_core::budget::{BudgetScheduler, QuotaProfiles};
use partiverse_core::error::PartisyError;
use partiverse_core::jobs::{JobManager, RcDispatch};
use partiverse_engine::client::{RcClient, RcMethod};
use partiverse_engine::coordinator::{EngineCoordinator, EngineHandle};
use serde_json::Value;

use crate::error::{CmdError, ErrorKind, Severity};

/// 互斥锁中毒映射(生产路径禁 unwrap:持锁方 panic = 共享态不可信,Fatal 上浮)。
pub fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, CmdError> {
    mutex.lock().map_err(|poisoned| {
        CmdError::new(
            ErrorKind::Internal,
            Severity::Fatal,
            format!("shared state mutex poisoned: {poisoned}"),
        )
    })
}

/// 壳共享状态(Arc 字段便于克隆进 spawn_blocking 闭包;coordinator/engine
/// 串行经锁,engine=活动句柄 ensure 写入 shutdown 清除;jobs/预算器同库)。
#[derive(Clone)]
pub struct ShellState {
    pub coordinator: Arc<Mutex<EngineCoordinator>>,
    pub engine: Arc<Mutex<Option<EngineHandle>>>,
    pub jobs: Arc<JobManager>,
    pub budget: Arc<BudgetScheduler>,
}

impl ShellState {
    /// 组装(启动路径):jobs/预算库按 core 默认安放,协调器按内嵌 manifest
    /// 构造;任一失败即启动失败上浮(fail-fast,零静默降级)。
    pub fn new() -> Result<Self, CmdError> {
        let jobs = JobManager::open_default()?;
        let budget = BudgetScheduler::open_default(QuotaProfiles::load_default()?)?;
        let coordinator = EngineCoordinator::new()?;
        Ok(Self {
            coordinator: Arc::new(Mutex::new(coordinator)),
            engine: Arc::new(Mutex::new(None)),
            jobs: Arc::new(jobs),
            budget: Arc::new(budget),
        })
    }

    /// 取活动引擎的 rc 分发适配器(引擎未拉起 → Fatal,前端须先 engine_ensure)。
    pub fn dispatch(&self) -> Result<ShellDispatch, CmdError> {
        let guard = lock(&self.engine)?;
        guard
            .as_ref()
            .map(|handle| ShellDispatch {
                client: handle.rc_client.clone(),
            })
            .ok_or_else(|| {
                CmdError::new(
                    ErrorKind::Engine,
                    Severity::Fatal,
                    "engine not running: call engine_ensure first",
                )
            })
    }
}

/// `RcClient` → `RcDispatch` 生产适配(白名单收口 + severity 保真;零透传)。
pub struct ShellDispatch {
    /// 活动引擎的 rc 白名单客户端(随句柄快照分发,凭据 Debug 脱敏)。
    client: RcClient,
}

impl RcDispatch for ShellDispatch {
    fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError> {
        let method = RcMethod::try_from_name(method)
            .map_err(|err| PartisyError::with_source(err.severity(), Box::new(err)))?;
        self.client
            .call(method, params)
            .map_err(|err| PartisyError::with_source(err.severity(), Box::new(err)))
    }
}
