//! 会话与 rc 分发适配(M1-WP07-T01 卡内 ③):`RcClient` → core `RcDispatch`
//! 适配器(壳 state.rs:92-100 形态,白名单收口 + severity 保真,零透传)+
//! 按次启动会话(`ensure → 执行 → shutdown on exit`;`RcdLaunch` Drop 兜底
//! kill,CLI 退出 = 引擎退出 = 运行中 job 终止,任务卡显式声明)。

use partiverse_core::error::PartisyError;
use partiverse_core::jobs::RcDispatch;
use partiverse_engine::coordinator::{EngineCoordinator, EngineHandle};
use partiverse_engine::slots::{DEFAULT_SLOT_ID, EngineSlotConfig};
use partiverse_engine::{EngineError, RcClient, RcMethod};
use serde_json::Value;

/// `RcClient` → `RcDispatch` 生产适配(卡内 ③,≈10 行;壳 `ShellDispatch`
/// 形态:白名单收口 + severity 保真,零透传)。
pub struct EngineDispatch {
    /// 活动引擎的 rc 白名单客户端(随句柄快照分发,凭据 Debug 脱敏)。
    client: RcClient,
}

impl RcDispatch for EngineDispatch {
    fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError> {
        let method = RcMethod::try_from_name(method)
            .map_err(|err| PartisyError::with_source(err.severity(), Box::new(err)))?;
        self.client
            .call(method, params)
            .map_err(|err| PartisyError::with_source(err.severity(), Box::new(err)))
    }
}

/// 按次启动会话:一次 CLI 调用内的引擎生命周期持有者(任务卡 ② 形态)。
pub struct Session {
    /// 持有协调器(rcd 子进程随 Drop 生命周期管理;显式 shutdown 走优雅退出,
    /// 失败时 Drop 兜底 kill——`RcdLaunch` Drop 契约)。
    coordinator: EngineCoordinator,
    /// 活动句柄快照(观测面:slot/pid/socket;ensure 返回时恒 Ready)。
    handle: EngineHandle,
    /// rc 分发适配器(由句柄 rc_client 快照构造;凭据 Debug 脱敏)。
    dispatch: EngineDispatch,
}

impl Session {
    /// 按默认槽位配置开启会话(生产入口:三级级联加载 default 槽位)。
    pub fn open() -> Result<Self, EngineError> {
        let slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID)?;
        Self::open_with_slot(&slot)
    }

    /// 以显式槽位配置开启会话(测试注入隔离 cache_dir 用;生产走 [`Self::open`])。
    pub fn open_with_slot(slot: &EngineSlotConfig) -> Result<Self, EngineError> {
        let mut coordinator = EngineCoordinator::new()?;
        let handle = coordinator.ensure(slot)?;
        let dispatch = EngineDispatch {
            client: handle.rc_client.clone(),
        };
        Ok(Session {
            coordinator,
            handle,
            dispatch,
        })
    }

    /// rc 分发适配器(命令层唯一 rc 通道)。
    #[must_use]
    pub fn dispatch(&self) -> &EngineDispatch {
        &self.dispatch
    }

    /// 引擎核心版本探活(透传协调器;无活动槽位/漂移 = Fatal 上浮,零吞错)。
    pub fn probe(&self) -> Result<String, EngineError> {
        self.coordinator.probe_core_version(DEFAULT_SLOT_ID)
    }

    /// 活动句柄快照观测面(engine ensure 输出用;非凭据字段)。
    #[must_use]
    pub fn handle(&self) -> &EngineHandle {
        &self.handle
    }

    /// 优雅退出(SIGTERM 2s 窗口 + socket 清理断言;失败时调用方 Drop 兜底
    /// kill——`RcdLaunch` Drop 契约,进程零残留)。
    pub fn shutdown(&mut self) -> Result<(), EngineError> {
        self.coordinator.shutdown(DEFAULT_SLOT_ID)
    }
}
