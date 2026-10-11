//! M1-WP99-T03 DoD ⑤ CLI 全链集成测试(真 rclone 1.75.1,离线零真实网络):
//! `pv config encrypt` 生产函数 + 会话注入链路三态(明文提示 / 注入探活 /
//! 缺钥 Fatal)。HOME+XDG_CONFIG_HOME 隔离到临时目录(密钥文件与默认 config
//! 全部落沙箱,零触碰用户真实面);PARTIVERSE_ENGINE_ROOT 锚回真实安装根
//! (防 HOME 隔离误触下载);进程内串行 + env 原值保存恢复(本二进制唯一
//! 测试,零并行读者)。纪律照 real_engine_offline.rs(预装预检防误触下载)。
// unix socket 门:引擎协调器仅 unix(同 real_engine_offline 口径)。
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use partiverse_cli::config as cli_config;
use partiverse_cli::connection;
use partiverse_cli::session::{self, ENCRYPT_HINT, Session};
use partiverse_core::credential_store;
use partiverse_core::error::Severity;
use partiverse_engine::rcd::resolve_default_config_path;
use partiverse_engine::{DEFAULT_SLOT_ID, EngineInstaller, EngineManifest, EngineSlotConfig};

/// 集成测试进程内串行(共享 env 与引擎安装根,real_engine_offline 同款纪律)。
fn test_serial_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 预装引擎预检(防误触网络下载;缺失 → 显式失败+安装指引)。
fn require_preinstalled_engine() -> PathBuf {
    let root = EngineInstaller::resolve_install_root().expect("engine install root resolvable");
    let manifest = EngineManifest::embedded().expect("embedded manifest parses");
    let asset = manifest.current_platform().expect("host platform asset");
    let binary = root.join(&manifest.version).join(&asset.binary_name);
    assert!(
        binary.is_file(),
        "rclone missing at {}; run `cargo run -p partiverse-engine --bin engine-install`",
        root.display()
    );
    binary
}

/// 槽位(cache_dir 落沙箱;engine_root 由 env 锚定真实安装根)。
fn test_slot(work: &Path) -> EngineSlotConfig {
    let mut slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("default slot loads");
    slot.cache_dir = work.join("cache");
    fs::create_dir_all(&slot.cache_dir).expect("create test cache dir");
    slot
}

#[test]
fn encrypt_roundtrip_injected_session_probe_and_missing_key_fail_closed() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    // env 覆写前锚定真实引擎面(隔离后仍指向真实安装根,防误触下载)。
    let rclone = require_preinstalled_engine();
    let real_engine_root = EngineInstaller::resolve_install_root().expect("real engine root");

    let work = std::env::temp_dir().join(format!("partiverse-cli-wp99-t03-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("create sandbox work dir");
    let saved_home = std::env::var_os("HOME");
    let saved_xdg = std::env::var_os("XDG_CONFIG_HOME");
    let saved_root = std::env::var_os("PARTIVERSE_ENGINE_ROOT");
    let saved_slots = std::env::var_os("PARTIVERSE_ENGINE_SLOTS_FILE");
    // SAFETY:本测试二进制仅此一测且持串行锁,env 零并行读者。
    unsafe {
        std::env::set_var("HOME", &work);
        std::env::set_var("XDG_CONFIG_HOME", work.join(".config"));
        std::env::set_var("PARTIVERSE_ENGINE_ROOT", &real_engine_root);
        std::env::remove_var("PARTIVERSE_ENGINE_SLOTS_FILE");
    }
    let restore = |work: &Path| {
        // SAFETY:同上,串行锁内恢复原值。
        unsafe {
            match &saved_home {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
            match &saved_xdg {
                Some(dir) => std::env::set_var("XDG_CONFIG_HOME", dir),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match &saved_root {
                Some(root) => std::env::set_var("PARTIVERSE_ENGINE_ROOT", root),
                None => std::env::remove_var("PARTIVERSE_ENGINE_ROOT"),
            }
            match &saved_slots {
                Some(file) => std::env::set_var("PARTIVERSE_ENGINE_SLOTS_FILE", file),
                None => std::env::remove_var("PARTIVERSE_ENGINE_SLOTS_FILE"),
            }
        }
        let _ = fs::remove_dir_all(work);
    };
    // 恢复守卫:任一 expect 失败也必须还原 env(零副作用泄漏出测试进程)。
    let guard_work = work.clone();
    let scope = std::panic::AssertUnwindSafe(|| {
        run_roundtrip(&rclone, &work);
    });
    let result = std::panic::catch_unwind(scope);
    restore(&guard_work);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// 测试主体(env 隔离已就位;DoD ①②③④⑤ 全链)。
fn run_roundtrip(rclone: &Path, work: &Path) {
    // ── ① 明文/缺失面:无注入 + 提示文案锚定(卡内 ②)──
    assert!(
        ENCRYPT_HINT.contains("run `pv config encrypt` to enable"),
        "提示须含卡内原文: {ENCRYPT_HINT}"
    );
    let plaintext = session::default_config_injection(rclone).expect("plaintext resolves");
    assert!(
        plaintext.is_none(),
        "明文/缺失 config 必须无注入启动: {plaintext:?}"
    );

    // ── ② 明文会话(无注入)建连:remote 落隔离默认 config(迁移素材)──
    let slot = test_slot(work);
    let data_dir = work.join("data");
    fs::create_dir_all(&data_dir).expect("create fixture data dir");
    fs::write(data_dir.join("seed.txt"), b"partiverse cli wp99-t03").expect("write fixture file");
    let mut plain_session = Session::open_with_slot(&slot).expect("plaintext session opens");
    connection::create_local(
        plain_session.dispatch(),
        "pv-t03-migrated",
        &data_dir.to_string_lossy(),
    )
    .expect("create-local ok on plaintext config");
    plain_session
        .shutdown()
        .expect("plaintext session shutdown");

    // ── ③ `pv config encrypt` 生产函数全链(卡内 ①)──
    let out = cli_config::encrypt().expect("config encrypt ok");
    assert!(
        out.contains("created") && out.contains("0600") && out.contains("verified"),
        "encrypt 输出须含密钥文件状态+0600+可解密断言: {out}"
    );
    let key_path = credential_store::master_key_file_path().expect("key path resolvable");
    assert_eq!(
        key_path,
        work.join(".config").join("partiverse").join("master.key"),
        "密钥文件固定路径(卡内边界): {}",
        key_path.display()
    );
    assert!(key_path.is_file(), "master.key must exist");
    let mode = fs::metadata(&key_path)
        .expect("key metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "0600 强制(卡内 ①)");
    let config_path =
        resolve_default_config_path(rclone).expect("default config path under sandbox");
    assert!(
        config_path.starts_with(work),
        "config 路径必须落在沙箱(零触碰用户真实面): {}",
        config_path.display()
    );
    assert!(
        credential_store::config_is_encrypted(&config_path).expect("encrypted probe"),
        "encrypt 后默认 config 必须已加密: {}",
        config_path.display()
    );

    // ── ④ 幂等面(卡内 ①「已加密→幂等提示」)──
    let again = cli_config::encrypt().expect("idempotent encrypt ok");
    assert!(
        again.contains("already enabled"),
        "二次 encrypt 必须幂等提示: {again}"
    );

    // ── ⑤ 加密往返:注入会话启动 → 探活 → 迁移保真(remote 仍在,ls 可用)──
    let mut encrypted_session =
        Session::open_with_slot(&slot).expect("injected session opens (encrypted config)");
    let version = encrypted_session.probe().expect("probe under injection");
    assert!(version.starts_with('v'), "core version shape: {version}");
    let listing = connection::list(encrypted_session.dispatch()).expect("connection list ok");
    assert!(
        listing
            .lines()
            .any(|line| line.starts_with("pv-t03-migrated\talias")),
        "迁移保真:既有连接必须在注入会话中可见: {listing}"
    );
    encrypted_session
        .shutdown()
        .expect("injected session shutdown");

    // ── ⑥ master.key 丢失 → 显式 Fatal「master key missing: config
    //    undecryptable」,禁止降级明文启动(卡内 ④)──
    fs::remove_file(&key_path).expect("remove master key file");
    let err = match Session::open_with_slot(&slot) {
        Ok(_) => panic!("missing key must fail closed, not open a plaintext session"),
        Err(err) => err,
    };
    assert_eq!(err.severity(), Severity::Fatal);
    let chain = partiverse_engine::error::error_chain(&err);
    assert!(
        chain.contains("master key missing: config undecryptable"),
        "DoD ④ 原文短语必须可见: {chain}"
    );
}
