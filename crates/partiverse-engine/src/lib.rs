//! partiverse-engine:rclone rcd 引擎协调器(生命周期/健康探针/引擎热更新)。
//!
//! M1-WP02-T01 交付引擎分发器:锁定版本下载 / sha256 校验 / 多源回退 / 手动
//! 导入(D31:镜像只换传输不换校验);T02/T03 续落地 rcd spawn 与槽位编排。
//! 依赖集与 license 审计见 docs/adr/0004*.md。

pub mod error;
pub mod hash;
pub mod installer;
pub mod manifest;
pub mod source;

pub use error::{EngineError, EngineErrorKind};
pub use installer::EngineInstaller;
pub use manifest::EngineManifest;
