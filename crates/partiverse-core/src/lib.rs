//! partiverse-core:Partiverse 领域核心(统一索引/配额预算/传输编排的公共底座)。
//!
//! 当前为 M1-WP01-T01 交付的骨架:仅版本常量与空模块树,不含任何业务逻辑;
//! `error`/`caps` 经批准自 partisync `d7f73ffd516a` 拷贝(见各自文件头的来源标注)。

/// crate 版本(占位;由 Cargo.toml 注入,随版本纪律 D37 联动)。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod budget;
pub mod caps;
// 模块名 credential_store(而非 credentials):仓库 .gitignore 既有 `credentials*`
// 反密钥入库模式会吞掉 credentials.rs 致其永远无法提交,改名绕开(M1-WP04-T01)。
pub mod credential_store;
pub mod error;
pub mod jobs;
// OAuth 本地回调与百度 oob 式本地换码(M1-WP04-T02):回调服务器/oob 回填
// 换码/token 下沉接缝,核心零 token 持久化。
pub mod oauth;
// 123 云盘官方 WebDAV 接入流(M1-WP04-T03):端点/账号/应用专用密码 → webdav
// vendor remote 描述;端点用户粘贴零硬编码,应用专用密码全程脱敏。
pub mod pan123;
// rc `config/providers` schema → 表单字段描述模型 + `config/create` 提交通道
// (M1-WP04-T03):模型零 UI 概念(React 实件属 WP05),向导直接渲染。
pub mod schema;

/// 数据模型空壳:由 WP08(索引与搜索)的数据模型落地,当前刻意留空。
pub mod types;
