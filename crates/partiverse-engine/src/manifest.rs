//! engine-manifest.json 解析与校验(M1-WP02-T01)。
//!
//! manifest 随仓分发并编译期内嵌(`include_str!`):下载源 URL 一律从 manifest
//! 读取,代码零硬编码端点(D31:镜像只换传输不换校验)。解析时即做字段级校验
//! (哈希形状/路径安全/HTTPS 强制),非法 manifest 视为 Fatal 数据问题。

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::{EngineError, EngineErrorKind};
use crate::hash::decode;

/// 随仓分发的 engine-manifest.json(编译期内嵌,运行期零外部文件依赖)。
pub const EMBEDDED_MANIFEST_JSON: &str = include_str!("../manifest/engine-manifest.json");

/// 引擎发行 manifest:锁定版本 + 五平台构件 + 有序下载源。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EngineManifest {
    /// 锁定的引擎版本(如 "1.75.1");同时用作安装目录名,禁路径分隔符。
    pub version: String,
    /// 平台键 → 构件描述(平台键如 `linux-amd64`,见 `host_platform_key`)。
    pub platforms: BTreeMap<String, PlatformAsset>,
    /// 下载源有序列表:主源在前、备源在后,按序回退(D31)。
    pub sources: Vec<DownloadSource>,
}

/// 单平台构件描述。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlatformAsset {
    /// 官方发行包 zip 文件名(如 `rclone-v1.75.1-linux-amd64.zip`)。
    pub zip_name: String,
    /// 官方 SHA256SUMS 中该 zip 的 sha256(小写 hex)。
    pub sha256: String,
    /// 解包后的引擎二进制文件名(unix=`rclone`,windows=`rclone.exe`)。
    pub binary_name: String,
}

/// 单个下载源描述(name=诊断标识;base_url=强制 HTTPS 的源基址)。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DownloadSource {
    /// 源名(诊断输出与多源尝试记录使用)。
    pub name: String,
    /// 源基址(强制 HTTPS;完整 URL = 去尾斜杠基址 + "/" + zip_name)。
    pub base_url: String,
}

impl EngineManifest {
    /// 从 JSON 文本解析并完成字段校验;非法输入返回 Fatal `ManifestInvalid`。
    pub fn from_json(json: &str) -> Result<Self, EngineError> {
        let manifest: Self = serde_json::from_str(json).map_err(|err| {
            EngineError::new(
                EngineErrorKind::ManifestInvalid(err.to_string()),
                partiverse_core::error::Severity::Fatal,
            )
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// 解析随仓内嵌 manifest(生产入口)。
    pub fn embedded() -> Result<Self, EngineError> {
        Self::from_json(EMBEDDED_MANIFEST_JSON)
    }

    /// 当前宿主平台键(如 `linux-amd64` / `osx-arm64` / `windows-amd64`)。
    #[must_use]
    pub fn host_platform_key() -> String {
        // std 常量到 manifest 平台键的映射:x86_64→amd64、aarch64→arm64、macos→osx。
        let os = match std::env::consts::OS {
            "macos" => "osx",
            other => other,
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            other => other,
        };
        format!("{os}-{arch}")
    }

    /// 取当前宿主平台构件;宿主平台不在表内 → Fatal `PlatformUnsupported`。
    pub fn current_platform(&self) -> Result<&PlatformAsset, EngineError> {
        let key = Self::host_platform_key();
        self.platforms.get(&key).ok_or_else(|| {
            EngineError::new(
                EngineErrorKind::PlatformUnsupported(key),
                partiverse_core::error::Severity::Fatal,
            )
        })
    }

    /// 字段级校验:路径安全(组件字符集)、哈希形状(64 位小写 hex)、源非空且强制 HTTPS。
    fn validate(&self) -> Result<(), EngineError> {
        let invalid = |detail: String| {
            EngineError::new(
                EngineErrorKind::ManifestInvalid(detail),
                partiverse_core::error::Severity::Fatal,
            )
        };
        if !is_safe_path_component(&self.version) {
            return Err(invalid(format!(
                "unsafe version component: {:?}",
                self.version
            )));
        }
        if self.platforms.is_empty() {
            return Err(invalid("platforms must not be empty".into()));
        }
        for (key, asset) in &self.platforms {
            if !is_safe_path_component(key) {
                return Err(invalid(format!("unsafe platform key: {key}")));
            }
            if decode(&asset.sha256).is_none() {
                return Err(invalid(format!(
                    "platform {key}: sha256 must be 64 lowercase hex chars"
                )));
            }
            if !is_safe_path_component(&asset.zip_name) {
                return Err(invalid(format!(
                    "platform {key}: unsafe zip_name {:?}",
                    asset.zip_name
                )));
            }
            if !is_safe_path_component(&asset.binary_name) {
                return Err(invalid(format!(
                    "platform {key}: unsafe binary_name {:?}",
                    asset.binary_name
                )));
            }
        }
        if self.sources.is_empty() {
            return Err(invalid("sources must not be empty".into()));
        }
        for source in &self.sources {
            if source.name.is_empty() {
                return Err(invalid("source name must not be empty".into()));
            }
            // 证书校验不可关(D31/任务卡边界):基址仅接受 HTTPS,无明文通道开关。
            if !source.base_url.starts_with("https://") || source.base_url.len() <= "https://".len()
            {
                return Err(invalid(format!(
                    "source {}: base_url must be an https url",
                    source.name
                )));
            }
        }
        Ok(())
    }
}

/// 路径组件安全判定:非空且仅限 `[A-Za-z0-9._+-]`(隐式拒绝分隔符与遍历形)。
fn is_safe_path_component(text: &str) -> bool {
    !text.is_empty()
        && text != "."
        && text != ".."
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_json() -> String {
        format!(
            r#"{{"version":"1.0","platforms":{{"{key}":{{"zip_name":"a.zip","sha256":"{sha}","binary_name":"rclone"}}}},"sources":[{{"name":"s","base_url":"https://example.test/v"}}]}}"#,
            key = EngineManifest::host_platform_key(),
            sha = "0".repeat(64)
        )
    }

    #[test]
    fn embedded_manifest_has_locked_version_five_platforms_and_ordered_sources() {
        // 任务卡 ①:随仓 manifest 锚定 1.75.1 + 五平台哈希 + 有序双源 + 宿主平台覆盖。
        let manifest = EngineManifest::embedded().expect("embedded manifest must parse");
        assert_eq!(manifest.version, "1.75.1");
        for key in [
            "linux-amd64",
            "linux-arm64",
            "windows-amd64",
            "osx-amd64",
            "osx-arm64",
        ] {
            assert!(
                manifest.platforms.contains_key(key),
                "missing platform {key}"
            );
            let sha = &manifest.platforms[key].sha256;
            assert_eq!(sha.len(), 64, "platform {key}: sha256 must be 64 hex chars");
            assert!(decode(sha).is_some(), "platform {key}: sha must be hex");
        }
        assert_eq!(
            manifest.sources.len(),
            2,
            "ordered sources: main + fallback"
        );
        assert_eq!(manifest.sources[0].name, "github-releases");
        assert_eq!(manifest.sources[1].name, "downloads-rclone-org");
        let key = EngineManifest::host_platform_key();
        assert!(
            manifest.platforms.contains_key(&key),
            "host platform {key} must be covered"
        );
        assert!(manifest.current_platform().is_ok());
    }

    #[test]
    fn invalid_manifests_are_rejected_as_fatal() {
        // 表驱动:路径遍历版本 / 坏哈希形状 / 明文基址 → 全部 Fatal ManifestInvalid。
        let cases: Vec<(&str, String)> = vec![
            (
                "path traversal version",
                valid_json().replace(r#""version":"1.0""#, r#""version":"../evil""#),
            ),
            (
                "bad sha shape",
                valid_json().replace(&"0".repeat(64), "xyz"),
            ),
            (
                "plaintext base_url",
                valid_json().replace("https://example.test", "http://example.test"),
            ),
        ];
        for (label, json) in cases {
            let err = EngineManifest::from_json(&json)
                .map(|_| ())
                .expect_err(label);
            assert_eq!(
                err.severity(),
                partiverse_core::error::Severity::Fatal,
                "{label}"
            );
            assert!(
                matches!(err.kind(), EngineErrorKind::ManifestInvalid(_)),
                "{label}"
            );
        }
    }

    #[test]
    fn unsupported_host_platform_surfaces_fatal() {
        // 任务卡 ⑥:平台表不含宿主键 → Fatal 上浮(ensure 第一步即拦截)。
        let json = r#"{"version":"1.0","platforms":{"solaris-sparcv9":{"zip_name":"a.zip","sha256":"0000000000000000000000000000000000000000000000000000000000000000","binary_name":"rclone"}},"sources":[{"name":"s","base_url":"https://example.test/v"}]}"#;
        let manifest = EngineManifest::from_json(json).expect("manifest shape is valid");
        let err = manifest
            .current_platform()
            .expect_err("host platform must be unsupported");
        assert_eq!(err.severity(), partiverse_core::error::Severity::Fatal);
        assert!(matches!(
            err.kind(),
            EngineErrorKind::PlatformUnsupported(_)
        ));
    }
}
