//! M1-WP04-T01 集成测试(真 rclone 1.75.1 离线 + 真 OS keychain):主密钥
//! keychain 生命周期(写→读回→轮换)→ 启用加密 config(主密钥仅经 stdin)→
//! 文件密文断言(`RCLONE_ENCRYPT_V0` 头 + 零明文密钥)→ rcd 经
//! `spawn_with_config` 注入启动探活 + `verify_config_decryptable`(正确密钥
//! Ok)→ 轮换后旧 config 以 Fatal 显式上浮(零静默)。rclone 解析与 engine
//! 测试同约定(env `PARTIVERSE_TEST_RCLONE_BIN` > 默认引擎根);取密面 =
//! 生产式可执行脚本(内容零密钥),keyctl 不可用时解密段显式跳过标注。
//! 真 keychain:linux=内核 keyring 本机实测;Windows/mac 仅编译门,
//! 行为**待 Owner 实测**(ADR-0007)。

use std::path::{Path, PathBuf};
use std::process::Command;

use partiverse_core::credential_store::{
    self, CredentialErrorKind, KeyringStore, OsKeyring, kind_of,
};
use partiverse_core::error::Severity;
use partiverse_engine::rcd::{ConfigInjection, RcdState, RcdSupervisor};

/// 测试二进制解析环境变量(与 engine 测试同约定)。
const TEST_RCLONE_ENV: &str = "PARTIVERSE_TEST_RCLONE_BIN";

/// 测试用 rclone 解析(env 覆盖 > 默认引擎根);缺失 → panic+安装指引,
/// 禁止静默 skip(与 engine::rcd 测试同口径)。
fn test_rclone_binary() -> PathBuf {
    if let Some(path) = std::env::var_os(TEST_RCLONE_ENV).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "{TEST_RCLONE_ENV} missing file: {}",
            path.display()
        );
        return path;
    }
    let install_root = partiverse_engine::installer::EngineInstaller::resolve_install_root()
        .expect("engine install root must be resolvable");
    let manifest =
        partiverse_engine::manifest::EngineManifest::embedded().expect("manifest must parse");
    let asset = manifest.current_platform().expect("host platform asset");
    let binary = install_root
        .join(&manifest.version)
        .join(&asset.binary_name);
    assert!(
        binary.is_file(),
        "test rclone binary missing at {}; install it with \
         `cargo run -p partiverse-engine --bin engine-install` \
         or set {TEST_RCLONE_ENV} to the rclone binary",
        binary.display()
    );
    binary
}

/// 生产式取密脚本落地(unix;内容与 config/rclone-password-command.sh 同构,
/// service/account 随测试隔离命名):keyctl CLI/内核 keyring 不可用 → None。
fn materialize_password_script(dir: &Path, service: &str, account: &str) -> Option<PathBuf> {
    let probe = Command::new("keyctl").arg("show").arg("@s").output();
    match probe {
        Ok(status) if status.status.success() => {}
        _ => return None,
    }
    let script = dir.join("rclone-password-command.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             ID=$(keyctl search @s user 'keyring-rs:{account}@{service}') || exit 1\n\
             exec keyctl pipe \"$ID\"\n"
        ),
    )
    .ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    Some(script)
}

#[test]
fn config_encryption_full_lifecycle_with_zero_plaintext_and_explicit_rotation_failure() {
    let rclone = test_rclone_binary();
    // 测试专用 keychain 命名(与生产常量隔离,防触碰真实用户数据)。
    let (service, account) = ("partiverse-it-wp04", "rclone-config");
    let store = OsKeyring::with_names(service, account).expect("keyring entry constructs");
    store.delete().expect("pre-clean keychain entry");
    let dir = std::env::temp_dir().join(format!("partiverse-wp04-t01-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create test dir");
    let config = dir.join("rclone.conf");

    // 卡内 ①④:keychain 生命周期 = ensure(缺键生成+写+读回)。
    let key = credential_store::ensure_master_key(&store).expect("master key ensured");
    assert_eq!(store.get().expect("read back"), key, "keychain 读回一致");

    // 卡内 ③:启用加密 config(真 rclone;密钥仅经 stdin)。
    credential_store::enable_config_encryption(&rclone, &config, &key)
        .expect("config encryption enabled");
    let encrypted = std::fs::read_to_string(&config).expect("config readable");
    assert!(
        encrypted.contains("RCLONE_ENCRYPT_V0:"),
        "config file must carry the encrypted marker"
    );
    // 卡内 ⑤ 零明文审计:文件为密文,明文主密钥不得出现。
    assert!(
        !encrypted.contains(&key),
        "master key must never appear in plaintext in the config file"
    );

    // keyctl 不可用 → 解密相关段显式跳过标注(禁静默),密文审计仍执行。
    let Some(script) = materialize_password_script(&dir, service, account) else {
        println!(
            "SKIP(note): keyctl CLI/kernel keyring unavailable; \
             decryption/rotation/rcd segments not exercised"
        );
        store.delete().expect("cleanup keychain entry");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };

    // 卡内 ③:rcd 带注入启动(加密 config + RCLONE_PASSWORD_COMMAND 取密脚本)
    // → 就绪探活成功(rclone 1.75.1 config 惰性加载,实机核实;可解密性由
    // verify_config_decryptable 显式把关,探活不虚报)。
    let injection = ConfigInjection {
        config_path: config.clone(),
        password_command: script.to_string_lossy().into_owned(),
    };
    let mut supervisor =
        RcdSupervisor::spawn_with_config(&rclone, &dir.join("cache"), Some(&injection))
            .expect("rcd with encrypted config reaches ready");
    assert_eq!(supervisor.state(), RcdState::Ready);
    supervisor
        .verify_config_decryptable()
        .expect("config decryptable under injection");

    // 卡内 ③「无密钥启动失败」面 + 卡内 ④⑤:轮换(删除+重生成)后,取密脚本
    // 解出新键,旧 config 解密失败 → Fatal 显式上浮(零静默)。
    let new_key = credential_store::rotate_master_key(&store).expect("rotation");
    assert_ne!(key, new_key, "rotation must replace the master key");
    let engine_err = supervisor
        .verify_config_decryptable()
        .expect_err("rotated key must not decrypt old config");
    assert_eq!(engine_err.severity(), Severity::Fatal);
    assert!(matches!(
        engine_err.kind(),
        partiverse_engine::error::EngineErrorKind::RcdConfigDecryptFailed(_)
    ));
    let core_err =
        credential_store::verify_config_decryptable(&rclone, &config, &script.to_string_lossy())
            .expect_err("core-side verify must fail explicitly after rotation");
    assert_eq!(core_err.severity, Severity::Fatal);
    assert!(matches!(
        kind_of(&core_err),
        Some(CredentialErrorKind::ConfigDecryptFailed(_))
    ));

    supervisor.shutdown().expect("graceful shutdown");
    store.delete().expect("cleanup keychain entry");
    let _ = std::fs::remove_dir_all(&dir);
}
