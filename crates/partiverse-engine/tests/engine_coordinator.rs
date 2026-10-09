//! M1-WP02-T03 卡⑥集成测试(真实 rclone rcd):ensure(default)→ `core/version`
//! 探活(=1.75.1)→ shutdown → socket 清理;ensure 幂等断言;单实例结构预留
//! 断言(第二槽位拒绝)。前置:rclone 1.75.1 预装默认引擎根(T01 安装入口或
//! CI 预置);缺失 → 显式断言失败 + 安装指引(禁静默 skip,T02 同款纪律)。

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use partiverse_core::error::Severity;
use partiverse_engine::error::EngineErrorKind;
use partiverse_engine::rcd::RcdState;
use partiverse_engine::{
    DEFAULT_SLOT_ID, EngineCoordinator, EngineInstaller, EngineManifest, EngineSlotConfig,
};
use sha2::{Digest, Sha256};

/// 两个集成测试共享同一引擎根(ensure 的安装/自愈写同一组文件),进程内串行,
/// 杜绝并发双装踩踏共享临时文件(`<binary>.part`)。
fn test_serial_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 预装引擎预检(卡内素材契约,防测试误触网络下载慢路径):二进制存在且
/// `.sha256` 留档与实测一致(T01 快路径同一强度);不满足 → 显式断言失败 +
/// 安装指引(禁静默 skip/静默联网,T02 同款纪律)。
fn require_preinstalled_engine() {
    let root = EngineInstaller::resolve_install_root().expect("engine install root resolvable");
    let manifest = EngineManifest::embedded().expect("embedded manifest parses");
    let asset = manifest.current_platform().expect("host platform asset");
    let binary = root.join(&manifest.version).join(&asset.binary_name);
    assert!(
        binary.is_file(),
        "rclone not preinstalled at {}; run `cargo run -p partiverse-engine --bin engine-install` \
         first or provision the default engine root",
        binary.display()
    );
    let mut sidecar_path = binary.clone().into_os_string();
    sidecar_path.push(".sha256");
    let recorded = fs::read_to_string(&sidecar_path).unwrap_or_else(|err| {
        panic!(
            "engine sidecar unreadable at {} ({err}); provision it with \
             `cargo run -p partiverse-engine --bin engine-install`",
            sidecar_path.to_string_lossy()
        )
    });
    let bytes = fs::read(&binary).expect("read preinstalled engine binary");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hex::encode(hasher.finalize());
    assert_eq!(
        recorded.trim(),
        actual,
        "preinstalled engine binary fails sha256 re-verification against its sidecar; \
         re-provision with `cargo run -p partiverse-engine --bin engine-install`"
    );
}

/// 测试专用槽位 cache 目录(pid 唯一化;收尾清理)。
fn test_cache_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "partiverse-coordinator-cache-{tag}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create test cache dir");
    dir
}

/// 加载随仓默认槽位(内嵌默认文件即测试素材),cache_dir 改指临时目录
/// (测试卫生:不写真实用户数据目录;engine_root 保持默认 = 真实预装根)。
fn test_default_slot(tag: &str) -> (EngineSlotConfig, PathBuf) {
    let mut slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("default slot loads");
    let cache = test_cache_dir(tag);
    slot.cache_dir = cache.clone();
    (slot, cache)
}

#[test]
fn default_slot_lifecycle_ensure_probe_shutdown_and_idempotent_ensure() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    require_preinstalled_engine();
    let (slot, cache) = test_default_slot("lifecycle");
    let mut coordinator = EngineCoordinator::new().expect("coordinator constructs");
    // 首次 ensure:安装器(快路径 sha256 复验)→ spawn(--cache-dir 注入)→ Ready。
    let handle1 = coordinator.ensure(&slot).expect("ensure default slot");
    assert_eq!(handle1.slot_id, DEFAULT_SLOT_ID);
    assert_eq!(handle1.state, RcdState::Ready);
    assert!(handle1.pid.is_some(), "engine pid available while ready");
    assert!(
        handle1.socket_path.exists(),
        "unix socket exists while running"
    );
    assert!(cache.is_dir(), "slot cache dir materialized by ensure");
    // 卡内 ⑥:`core/version` 探活 = 1.75.1(manifest 锁定版本)。
    let expected_version = format!("v{}", EngineManifest::embedded().expect("manifest").version);
    let reported = coordinator
        .probe_core_version(DEFAULT_SLOT_ID)
        .expect("core/version probe");
    assert_eq!(reported, expected_version, "locked engine version");
    // 幂等:同 id 同配置重复 ensure → 同 pid/socket,零重建。
    let handle2 = coordinator.ensure(&slot).expect("repeated ensure");
    assert_eq!(handle1, handle2, "ensure must be idempotent while ready");
    // shutdown:SIGTERM 优雅退出 + socket 清理断言(协调器内断言 + 此处双验)。
    coordinator.shutdown(DEFAULT_SLOT_ID).expect("shutdown");
    assert!(
        !handle2.socket_path.exists(),
        "socket cleaned after shutdown"
    );
    // 活动槽位已清:重复 shutdown → 显式 Fatal(非静默成功)。
    let err = coordinator
        .shutdown(DEFAULT_SLOT_ID)
        .expect_err("double shutdown must fail");
    assert_eq!(err.severity(), Severity::Fatal);
    assert!(matches!(
        err.kind(),
        EngineErrorKind::CoordinatorInvalidState(_)
    ));
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn second_slot_ensure_is_rejected_while_single_instance_active() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    require_preinstalled_engine();
    let (slot, cache) = test_default_slot("single");
    let mut coordinator = EngineCoordinator::new().expect("coordinator constructs");
    let handle = coordinator.ensure(&slot).expect("ensure default slot");
    // 单实例结构预留(Q5):活动槽位存在时 ensure 其他槽位 → Fatal,不并发。
    let mut pro = slot.clone();
    pro.id = "pro".into();
    pro.remote_namespace = "pvp".into();
    pro.cache_dir = test_cache_dir("single-pro");
    let err = coordinator.ensure(&pro).expect_err("second slot must fail");
    assert_eq!(err.severity(), Severity::Fatal);
    assert!(matches!(
        err.kind(),
        EngineErrorKind::CoordinatorInvalidState(_)
    ));
    // 原槽位不受拒绝影响:仍幂等可探活,并可正常优雅退出。
    let handle_again = coordinator.ensure(&slot).expect("original slot intact");
    assert_eq!(handle, handle_again);
    coordinator.shutdown(DEFAULT_SLOT_ID).expect("shutdown");
    assert!(!handle.socket_path.exists(), "socket cleaned");
    let _ = fs::remove_dir_all(&cache);
    let _ = fs::remove_dir_all(&pro.cache_dir);
}
