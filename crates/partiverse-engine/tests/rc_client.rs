//! M1-WP03-T01 卡⑤集成测试(真实 rclone 1.75.1,离线):ensure(default slot)
//! → `EngineHandle.rc_client` 调 `core/version` 成功 + 白名单外本地拒绝(Fatal
//! 含被拒名)+ 错误映射真机锚定(config/get 缺参 → 400 → Fatal)。前置:rclone
//! 预装默认引擎根(缺失 → 显式失败,禁静默 skip;禁真实网络下载)。

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use partiverse_core::error::Severity;
use partiverse_engine::error::EngineErrorKind;
use partiverse_engine::rcd::RcdState;
use partiverse_engine::{
    DEFAULT_SLOT_ID, EngineCoordinator, EngineInstaller, EngineManifest, EngineSlotConfig, RcMethod,
};
use sha2::{Digest, Sha256};

/// 集成测试进程内串行(WP02 遗留:installer 共享路径并发踩踏,串行规避,
/// Owner 预飞须知第 3 条口径)。
fn test_serial_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 预装引擎预检(防测试误触网络下载):二进制在且 `.sha256` 与实测一致;
/// 不满足 → 显式失败 + 安装指引(禁静默 skip)。
fn require_preinstalled_engine() {
    let root = EngineInstaller::resolve_install_root().expect("engine install root resolvable");
    let manifest = EngineManifest::embedded().expect("embedded manifest parses");
    let asset = manifest.current_platform().expect("host platform asset");
    let binary = root.join(&manifest.version).join(&asset.binary_name);
    assert!(
        binary.is_file(),
        "rclone not preinstalled at {}; run `cargo run -p partiverse-engine --bin engine-install`",
        binary.display()
    );
    let mut sidecar_path = binary.clone().into_os_string();
    sidecar_path.push(".sha256");
    let recorded = fs::read_to_string(&sidecar_path).unwrap_or_else(|err| {
        panic!(
            "sidecar unreadable at {sidecar:?} ({err})",
            sidecar = sidecar_path.display()
        )
    });

    let bytes = fs::read(&binary).expect("read preinstalled engine binary");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hex::encode(hasher.finalize());
    assert_eq!(
        recorded.trim(),
        actual,
        "sha256 mismatch; re-provision engine"
    );
}

/// 加载随仓默认槽位(cache_dir 改指 pid 唯一化临时目录,不写真实用户目录)。
fn test_default_slot(tag: &str) -> (EngineSlotConfig, PathBuf) {
    let mut slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("default slot loads");
    let cache = std::env::temp_dir().join(format!(
        "partiverse-rc-client-cache-{tag}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&cache).expect("create test cache dir");
    slot.cache_dir = cache.clone();
    (slot, cache)
}

#[test]
fn rc_client_core_version_whitelist_rejection_and_error_mapping_on_real_engine() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    require_preinstalled_engine();
    let (slot, cache) = test_default_slot("real-engine");
    let mut coordinator = EngineCoordinator::new().expect("coordinator constructs");
    let handle = coordinator.ensure(&slot).expect("ensure default slot");
    assert_eq!(handle.state, RcdState::Ready);
    // 卡内 ④⑥:句柄暴露 RcClient,经 unix socket + 随机凭据 POST 调通
    // core/version,版本 = manifest 锁定版本(零 TCP)。
    let expected_version = format!("v{}", EngineManifest::embedded().expect("manifest").version);
    let payload = handle
        .rc_client
        .call(RcMethod::CoreVersion, &serde_json::json!({}))
        .expect("core/version over unix socket");
    assert_eq!(
        payload.get("version").and_then(|value| value.as_str()),
        Some(expected_version.as_str())
    );
    // 卡内 ②⑤:白名单外本地拒绝(Fatal 含被拒名,零出本进程)。
    for rejected in ["core/command", "sync/bisync", "nope/method"] {
        let err = RcMethod::try_from_name(rejected).expect_err(rejected);
        assert_eq!(err.severity(), Severity::Fatal, "{rejected}");
        assert!(matches!(
            err.kind(),
            EngineErrorKind::RcMethodRejected(name) if name == rejected
        ));
        assert!(err.to_string().contains(rejected), "{err}");
    }
    // 错误映射真机锚定:config/get 缺 name → 400 → Fatal RcClientError。
    let err = handle
        .rc_client
        .call(RcMethod::ConfigGet, &serde_json::json!({}))
        .expect_err("config/get without name must fail");
    assert_eq!(err.severity(), Severity::Fatal);
    let EngineErrorKind::RcClientError { method, status, .. } = err.kind() else {
        panic!("expect RcClientError, got {:?}", err.kind())
    };
    assert_eq!((method.as_str(), *status), ("config/get", 400));
    assert!(err.to_string().contains("Didn't find key"));
    // 凭据脱敏随句柄保持(RcClient 手动 Debug 输出 `<redacted>`)。
    assert!(format!("{handle:?}").contains("<redacted>"));
    coordinator.shutdown(DEFAULT_SLOT_ID).expect("shutdown");
    let _ = fs::remove_dir_all(&cache);
}
