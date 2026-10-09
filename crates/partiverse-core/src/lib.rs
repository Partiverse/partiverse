//! partiverse-core:Partiverse 领域核心(统一索引/配额预算/传输编排的公共底座)。
//!
//! 当前为 M1-WP01-T01 交付的骨架:仅版本常量与空模块树,不含任何业务逻辑;
//! `error`/`caps` 经批准自 partisync `d7f73ffd516a` 拷贝(见各自文件头的来源标注)。

/// crate 版本(占位;由 Cargo.toml 注入,随版本纪律 D37 联动)。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod caps;
pub mod error;

/// 数据模型空壳:由 WP08(索引与搜索)的数据模型落地,当前刻意留空。
pub mod types;
