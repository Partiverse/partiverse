//! 引擎协调器(M1-WP02-T03):槽位化生命周期编排——安装器(T01)→ supervisor
//! (T02,cache-dir 注入槽位参数)→ 探活 → Ready;shutdown 走 SIGTERM 优雅
//! 退出并断言 socket 清理。
//!
//! Q5 口径:单实例 rcd,协调器按「引擎槽位」设计(remote 命名空间/数据面端口/
//! cache-dir/engine_root 全参数化);多实例仅结构预留——活动槽位存在时 ensure
//! 其他槽位显式 Fatal(卡内边界:不实现多实例并发),扩展点 = `active` 单槽位
//! 改槽位表。`remote_namespace`/`data_port` 仅落参数面:remote 创建属 WP03/
//! WP04、数据面监听属 WP07,本卡零实现。cache_dir 于 ensure 时物化
//! (`create_dir_all`),shutdown 不删配置化的缓存目录(持久位置,删除属数据
//! 破坏);临时资源清理面 = unix socket。同步阻塞 API(ADR-0004 惯例):async
//! 调用方放 spawn_blocking。

use std::fs;
use std::path::PathBuf;

use partiverse_core::error::Severity;

use crate::error::{EngineError, EngineErrorKind};
use crate::installer::EngineInstaller;
use crate::rcd::{RcdState, RcdSupervisor};
use crate::slots::EngineSlotConfig;

/// 活动引擎槽位的就绪句柄快照(ensure 返回值;slot_id/pid/socket 构成幂等
/// 断言面)。 owned 快照而非引用:句柄生命周期与协调器解耦,便于上层持有。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineHandle {
    /// 槽位 id。
    pub slot_id: String,
    /// 引擎进程状态(ensure 返回时恒为 Ready)。
    pub state: RcdState,
    /// 引擎进程 PID(已收尾 → None)。
    pub pid: Option<u32>,
    /// 本次实例的 unix socket 路径(WP03 rc 客户端连接要素之一)。
    pub socket_path: PathBuf,
}

/// 单实例活动槽位(协调器私有:槽位参数 + 监督器成对持有)。
struct ActiveSlot {
    /// ensure 时的槽位参数快照(配置漂移检测基准)。
    slot: EngineSlotConfig,
    /// rcd 监督器(T02:spawn/探活/优雅退出/崩溃退避重启)。
    supervisor: RcdSupervisor,
}

impl ActiveSlot {
    /// 生成句柄快照(取监督器观测面字段,零凭据泄漏)。
    fn handle(&self) -> EngineHandle {
        EngineHandle {
            slot_id: self.slot.id.clone(),
            state: self.supervisor.state(),
            pid: self.supervisor.pid(),
            socket_path: self.supervisor.socket_path().to_path_buf(),
        }
    }
}

/// 引擎协调器:单实例活动槽位持有者(引擎安装随取随校验,监督器随 ensure 创建)。
pub struct EngineCoordinator {
    /// 引擎安装器(T01:ensure 幂等,快路径 sha256 复验)。
    installer: EngineInstaller,
    /// 当前活动槽位(单实例:Some 期间拒绝其他槽位 ensure)。
    active: Option<ActiveSlot>,
}

impl EngineCoordinator {
    /// 以随仓内嵌 manifest 构造协调器(manifest 非法 → Fatal 上浮)。
    pub fn new() -> Result<Self, EngineError> {
        Ok(EngineCoordinator {
            installer: EngineInstaller::from_embedded_manifest()?,
            active: None,
        })
    }

    /// 确保槽位就绪(卡内 ③):幂等(同 id 同配置已 Ready → 直接返回句柄,期间
    /// 崩溃则复用 T02 退避重启);同 id 不同配置 = 运行中配置漂移,Fatal;其他
    /// id = 单实例冲突,Fatal。冷路径:cache_dir 物化 → 安装器幂等取二进制 →
    /// supervisor spawn(注入 `--cache-dir`)→ 就绪探活 → Ready。
    pub fn ensure(&mut self, slot: &EngineSlotConfig) -> Result<EngineHandle, EngineError> {
        // 手工构造的槽位与解析路径同一强度守卫(非法配置在触引擎前即拦截)。
        slot.validate()?;
        if let Some(active) = self.active.as_mut() {
            if active.slot.id == slot.id {
                if active.slot != *slot {
                    return Err(EngineError::new(
                        EngineErrorKind::CoordinatorInvalidState(format!(
                            "slot `{}` config drift while running",
                            slot.id
                        )),
                        Severity::Fatal,
                    ));
                }
                // 幂等路径:存活直返;意外退出则按指数退避重启后再返(T02)。
                active.supervisor.ensure_running()?;
                return Ok(active.handle());
            }
            return Err(EngineError::new(
                EngineErrorKind::CoordinatorInvalidState(format!(
                    "single-instance engine: slot `{}` active, refusing to ensure `{}`",
                    active.slot.id, slot.id
                )),
                Severity::Fatal,
            ));
        }
        // 槽位资源面:cache-dir 先物化(IO 失败按 classify_io 分级上浮),
        // 再交安装器(T01 幂等)与 supervisor(T02 探活至 Ready)。
        fs::create_dir_all(&slot.cache_dir).map_err(EngineError::from_io)?;
        let binary = self.installer.ensure(&slot.engine_root)?;
        let active = ActiveSlot {
            slot: slot.clone(),
            supervisor: RcdSupervisor::spawn(&binary, &slot.cache_dir)?,
        };
        let handle = active.handle();
        self.active = Some(active);
        Ok(handle)
    }

    /// 对活动槽位执行一次 `core/version` 探活,返回上报版本(如 "v1.75.1");
    /// id 不匹配或无活动槽位 = 状态机非法调用,Fatal。探活失败经 `RcdNotReady`
    /// (Retryable)上浮,零吞错。
    pub fn probe_core_version(&self, slot_id: &str) -> Result<String, EngineError> {
        match &self.active {
            Some(active) if active.slot.id == slot_id => active.supervisor.probe_core_version(),
            Some(active) => Err(EngineError::new(
                EngineErrorKind::CoordinatorInvalidState(format!(
                    "probe: slot `{slot_id}` requested but `{}` active",
                    active.slot.id
                )),
                Severity::Fatal,
            )),
            None => Err(EngineError::new(
                EngineErrorKind::CoordinatorInvalidState(format!(
                    "probe: no active slot (`{slot_id}` requested)"
                )),
                Severity::Fatal,
            )),
        }
    }

    /// 优雅退出槽位(卡内 ④):SIGTERM(2s 窗口,SIGKILL 兜底)+ socket 文件
    /// 清理,退出后断言 socket 不复存在(残留 = 清理失败,Fatal 上浮,零静默);
    /// 无活动槽位或 id 不匹配 → Fatal。cache_dir 为配置化持久位置,不随
    /// shutdown 删除(避免数据破坏;临时资源清理面 = unix socket)。
    pub fn shutdown(&mut self, slot_id: &str) -> Result<(), EngineError> {
        let active = match self.active.as_mut() {
            Some(active) if active.slot.id == slot_id => active,
            Some(active) => {
                return Err(EngineError::new(
                    EngineErrorKind::CoordinatorInvalidState(format!(
                        "shutdown: slot `{slot_id}` requested but `{}` active",
                        active.slot.id
                    )),
                    Severity::Fatal,
                ));
            }
            None => {
                return Err(EngineError::new(
                    EngineErrorKind::CoordinatorInvalidState(format!(
                        "shutdown: no active slot (`{slot_id}` requested)"
                    )),
                    Severity::Fatal,
                ));
            }
        };
        active.supervisor.shutdown()?;
        // 卡内 ④ 清理断言:优雅退出后 unix socket 必须消失。
        if active.supervisor.socket_path().exists() {
            return Err(EngineError::new(
                EngineErrorKind::RcdShutdownFailed(
                    "unix socket still present after shutdown".into(),
                ),
                Severity::Fatal,
            ));
        }
        self.active = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::EngineErrorKind as Kind;

    #[test]
    fn shutdown_without_active_slot_is_fatal() {
        // 卡内 ④ 边界:无活动槽位 shutdown → 显式 Fatal(非静默成功)。
        let mut coordinator = EngineCoordinator::new().expect("coordinator constructs");
        let err = coordinator.shutdown("default").expect_err("must fail");
        assert_eq!(err.severity(), Severity::Fatal);
        assert!(matches!(err.kind(), Kind::CoordinatorInvalidState(_)));
    }

    #[test]
    fn probe_without_active_slot_is_fatal() {
        // 卡内 ⑥ 边界:无活动槽位探活 → 显式 Fatal(非静默)。
        let coordinator = EngineCoordinator::new().expect("coordinator constructs");
        let err = coordinator
            .probe_core_version("default")
            .expect_err("must fail");
        assert_eq!(err.severity(), Severity::Fatal);
        assert!(matches!(err.kind(), Kind::CoordinatorInvalidState(_)));
    }
}
