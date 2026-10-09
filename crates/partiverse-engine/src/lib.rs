//! partiverse-engine:rclone rcd 引擎协调器(生命周期/健康探针/引擎热更新)。
//!
//! M1-WP02-T01 交付引擎分发器:锁定版本下载 / sha256 校验 / 多源回退 / 手动
//! 导入(D31:镜像只换传输不换校验);T02 交付 rcd sidecar 监督器(spawn /
//! 探活 / 优雅退出 / 崩溃退避重启);T03 续落地引擎槽位编排。
//! 依赖集与 license 审计见 docs/adr/0004*.md 与 docs/adr/0005*.md。

pub mod client;
pub mod coordinator;
pub mod error;
pub mod hash;
pub mod installer;
pub mod manifest;
pub mod rcd;
pub mod slots;
pub mod source;

pub use client::{RcClient, RcMethod};
pub use coordinator::{EngineCoordinator, EngineHandle};
pub use error::{EngineError, EngineErrorKind};
pub use installer::EngineInstaller;
pub use manifest::EngineManifest;
pub use rcd::{RcdState, RcdSupervisor};
pub use slots::{DEFAULT_SLOT_ID, EngineSlotConfig};
