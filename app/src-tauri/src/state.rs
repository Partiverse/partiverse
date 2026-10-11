//! 壳共享状态与 rc 分发适配(M1-WP05-T01)。
//! 壳薄纪律:状态仅聚合 crates 已有件,零业务逻辑;`ShellDispatch` 是 engine
//! `RcClient` → core `RcDispatch` 唯一接缝适配(白名单收口+severity 保真,
//! 形态对齐 core 集成测试 `RcAdapter`),错误全部映射 `CmdError` 上浮。

use std::sync::{Arc, Mutex, MutexGuard};

use partiverse_core::budget::{BudgetScheduler, QuotaProfiles};
use partiverse_core::error::PartisyError;
use partiverse_core::jobs::{JobManager, RcDispatch};
use partiverse_core::oauth::{OAuthTokenSink, TokenSet};
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
/// 串行经锁,engine=活动句柄 ensure 写入 shutdown 清除;jobs/预算器同库;
/// token_slot=InMemoryTokenSink 进程内存槽位,M1-WP05-T08)。
#[derive(Clone)]
pub struct ShellState {
    pub coordinator: Arc<Mutex<EngineCoordinator>>,
    pub engine: Arc<Mutex<Option<EngineHandle>>>,
    pub jobs: Arc<JobManager>,
    pub budget: Arc<BudgetScheduler>,
    /// 百度换码 token 的进程内存槽位(R2 凭据钉子:仅内存、零落盘零日志;
    /// 零 IPC 读路径——token 不过线、不入前端,消费方=后续加密 config 落点卡)。
    pub token_slot: Arc<Mutex<Option<TokenSet>>>,
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
            token_slot: Arc::new(Mutex::new(None)),
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

impl ShellDispatch {
    /// 测试构造面(仅测试构建):真引擎句柄的 rc 客户端直入适配器,令壳命令
    /// 同步体的真引擎测试与生产路径同消费面(零生产影响)。
    #[cfg(test)]
    pub(crate) fn for_tests(client: RcClient) -> Self {
        ShellDispatch { client }
    }
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

/// 壳面 OAuth token sink 过渡件(M1-WP05-T08,Owner 裁定口径):core
/// `OAuthTokenSink` 的壳内实现——换码所得 token 仅保留在进程内存(进程退出
/// 即失),满足 R2「凭据零落盘零日志」红线;**迁移目标 = backends-go 百度后端
/// 落地后的 rclone 加密 config 持久化(另立卡,D27 过渡件惯例)**。落点延后
/// 的根因事实(2026-10-10 实测,引擎钉定 rclone v1.75.1 发行件):rc
/// `config/create` 对未注册 backend 返回 500 `couldn't find backend for type
/// "baidu"`,加密 config 落点在此之前不可建成。
pub struct InMemoryTokenSink {
    /// 进程内存槽位(ShellState 持有同一 Arc;零 IPC 读路径,token 不过线)。
    slot: Arc<Mutex<Option<TokenSet>>>,
}

impl InMemoryTokenSink {
    /// 绑定 ShellState 持有的内存槽位(同一 Arc,生命周期=进程)。
    pub fn new(slot: Arc<Mutex<Option<TokenSet>>>) -> Self {
        InMemoryTokenSink { slot }
    }
}

impl OAuthTokenSink for InMemoryTokenSink {
    /// 写入一次换码所得 token(覆盖写:同会话再次换码以最新授权为准);
    /// 持锁失败(先前持锁方 panic = 共享态不可信)按 Fatal 显式上浮,零吞错。
    ///
    /// # Errors
    /// 槽位互斥锁中毒 → 结构化 Fatal。
    fn store_token(&self, token: &TokenSet) -> Result<(), PartisyError> {
        let mut guard = self.slot.lock().map_err(|poisoned| {
            PartisyError::with_source(
                partiverse_core::error::Severity::Fatal,
                Box::new(std::io::Error::other(format!(
                    "token slot mutex poisoned: {poisoned}"
                ))),
            )
        })?;
        *guard = Some(TokenSet {
            access_token: token.access_token.clone(),
            refresh_token: token.refresh_token.clone(),
            expires_in: token.expires_in,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 换码 token 样本(测试夹具值,零真实凭据)。
    fn sample_token() -> TokenSet {
        TokenSet {
            access_token: "at-fixture-value".to_owned(),
            refresh_token: "rt-fixture-value".to_owned(),
            expires_in: 2_592_000,
        }
    }

    /// 过渡件契约:槽位初始为空;store_token 写入;再次换码覆盖写(最新授权为准)。
    #[test]
    fn sink_stores_then_overwrites_in_memory_slot() {
        let slot: Arc<Mutex<Option<TokenSet>>> = Arc::new(Mutex::new(None));
        let sink = InMemoryTokenSink::new(Arc::clone(&slot));
        assert!(
            slot.lock().expect("initial slot").is_none(),
            "slot must start empty"
        );

        sink.store_token(&sample_token()).expect("first store");
        {
            let guard = slot.lock().expect("read slot");
            let stored = guard.as_ref().expect("token stored");
            assert_eq!(stored.access_token, "at-fixture-value");
            assert_eq!(stored.expires_in, 2_592_000);
        }

        let renewed = TokenSet {
            access_token: "at-renewed".to_owned(),
            refresh_token: "rt-renewed".to_owned(),
            expires_in: 1,
        };
        sink.store_token(&renewed).expect("second store");
        let guard = slot.lock().expect("read slot");
        assert_eq!(
            guard.as_ref().expect("token stored").access_token,
            "at-renewed"
        );
    }

    /// R2 红线:槽位 Debug 面(共享态观测面)零 token 明文(core TokenSet 脱敏
    /// Debug 同源);sink 自身仅持槽位句柄,不含值。
    #[test]
    fn slot_debug_surface_carries_no_token_plaintext() {
        let slot: Arc<Mutex<Option<TokenSet>>> = Arc::new(Mutex::new(None));
        InMemoryTokenSink::new(Arc::clone(&slot))
            .store_token(&sample_token())
            .expect("store");
        let debug = format!("{:?}", slot);
        assert!(
            !debug.contains("at-fixture-value"),
            "access_token leaked: {debug}"
        );
        assert!(
            !debug.contains("rt-fixture-value"),
            "refresh_token leaked: {debug}"
        );
    }

    /// 零吞错:槽位互斥锁中毒(持锁方 panic)→ store_token 结构化 Fatal 上浮。
    #[test]
    fn store_token_surfaces_poisoned_slot() {
        let slot: Arc<Mutex<Option<TokenSet>>> = Arc::new(Mutex::new(None));
        // 中毒成因=持锁方 panic:主线程先取锁再显式释放(持锁期间其余线程只会
        // 阻塞而非 panic);毒化线程拿到锁后持守卫 panic,join 以 Err 收敛。
        {
            let _hold = slot.lock().expect("hold guard then release");
        }
        let poisoner_slot = Arc::clone(&slot);
        let victim = std::thread::spawn(move || {
            let _guard = poisoner_slot.lock().expect("acquire before poisoning");
            panic!("poison the slot: panic while holding the guard");
        });
        assert!(
            victim.join().is_err(),
            "panicking thread must join as error"
        );
        let err = InMemoryTokenSink::new(Arc::clone(&slot))
            .store_token(&sample_token())
            .expect_err("poisoned slot must surface");
        assert_eq!(err.severity, partiverse_core::error::Severity::Fatal);
    }
}
