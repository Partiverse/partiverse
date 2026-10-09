//! sha256 hex 编解码(M1-WP02-T01)。
//!
//! 校验语义(D31):manifest 只锚定发行 zip 的 sha256;二进制自身摘要在落盘时
//! 实测并留档,供 `ensure()` 快路径复验(文件摘要实现在 installer.rs 唯一使用方处)。

/// 摘要 → 小写 hex。
#[must_use]
pub fn encode(digest: [u8; 32]) -> String {
    hex::encode(digest)
}

/// 64 位小写 hex → 32 字节摘要;非法输入返回 None(语义由调用方决定)。
#[must_use]
pub fn decode(text: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(text).ok()?;
    bytes.try_into().ok()
}
