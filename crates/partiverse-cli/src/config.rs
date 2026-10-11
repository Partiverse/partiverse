//! config 子命令(M1-WP99-T03):`pv config encrypt` —— rclone config 加密
//! 生产接线(Owner 选型 2026-10-11:主密钥文件 0600,不走 keychain)。
//! CLI 零业务逻辑:密钥文件生命周期/加密迁移/可解密断言全透传 core
//! `credential_store`,引擎二进制与默认 config 路径解析透传 engine。
//!
//! 流程(卡内 ①):未加密 → 主密钥文件就位(生成/复用+0600 强制)→
//! `enable_config_encryption`(密钥仅经 stdin,既有连接保真迁移)→
//! `verify_config_decryptable` 以引擎同款取密命令端到端断言;已加密 → 幂等
//! 提示(密钥文件缺失则 Fail-closed 显式错误,禁哑幂等掩盖损坏态)。
//! 密钥明文零入日志/argv/输出(红线;输出仅路径与状态词)。

use partiverse_core::credential_store;
use partiverse_core::error::Severity;
use partiverse_engine::rcd::resolve_default_config_path;
use partiverse_engine::{EngineError, EngineErrorKind, EngineInstaller};

/// 结构化 Fatal 构造(connection.rs::fatal 同款先例形态;载荷零密钥)。
fn fatal(message: impl Into<String>) -> EngineError {
    EngineError::new(
        EngineErrorKind::ConfigInjectionUnresolved(message.into()),
        Severity::Fatal,
    )
}

/// `pv config encrypt`:rclone config 加密生产接线(幂等;密钥文件 0600)。
///
/// # Errors
/// 引擎未安装/安装器失败原样上浮;config 路径解析失败 → `ConfigInjectionUnresolved`;
/// 已加密且密钥文件缺失 → Fatal(Fail-closed,DoD ④ 语义);core 加密供给/
/// 断言失败经 `ConfigInjectionUnresolved` 保真上浮(密钥零入错误面)。
pub fn encrypt() -> Result<String, EngineError> {
    // ① 引擎二进制(安装器幂等:快路径 sha256 复验;未安装显式失败上浮)。
    let installer = EngineInstaller::from_embedded_manifest()?;
    let root = EngineInstaller::resolve_install_root()?;
    let binary = installer.ensure(&root)?;
    // ② config 路径:rclone 官方解析(`rclone config file`),零硬编码(红线)。
    let config_path = resolve_default_config_path(&binary)?;
    // ③ 已加密 → 幂等;但密钥文件缺失 = 引擎不可解密的损坏态,Fail-closed
    // 显式错误而非哑幂等成功(DoD ④ 同语义,禁静默)。
    if credential_store::config_is_encrypted(&config_path).map_err(map_credential_error)? {
        let key_path = credential_store::master_key_file_path().map_err(map_credential_error)?;
        if !key_path.is_file() {
            return Err(fatal(format!(
                "master key missing: config undecryptable (config already encrypted; \
                 restore the master key file: {})",
                key_path.display()
            )));
        }
        return Ok(format!(
            "config encryption already enabled (idempotent): {}\nmaster key file: {}",
            config_path.display(),
            key_path.display()
        ));
    }
    // ④ 主密钥文件就位:生成(unix=/dev/urandom,credential_store 复用)/复用
    // + 0600 强制(权限漂移自动修复,状态位如实呈现为警告)。
    let (status, key) = credential_store::ensure_master_key_file().map_err(map_credential_error)?;
    // ⑤ 启用加密:密钥两行仅经 stdin;既有 remotes 保真迁移(core 实锚修复)。
    credential_store::enable_config_encryption(&binary, &config_path, &key)
        .map_err(map_credential_error)?;
    // ⑥ 端到端断言:以引擎启动同款取密命令(`cat "<密钥文件>"`)校验可解密。
    let password_command = credential_store::master_key_password_command(&status.path);
    credential_store::verify_config_decryptable(&binary, &config_path, &password_command)
        .map_err(map_credential_error)?;
    // 输出面(英文;仅路径与状态词,密钥零入)。
    let key_state = if status.created { "created" } else { "reused" };
    let mut report = format!(
        "master key file {key_state}: {} (mode 0600)\nconfig encrypted: {}\n\
         decryptable via master key file: verified",
        status.path.display(),
        config_path.display()
    );
    if status.permission_repaired {
        report.push_str(&format!(
            "\nwarning: master key file mode repaired to 0600: {}",
            status.path.display()
        ));
    }
    Ok(report)
}

/// core `PartisyError` → engine `EngineError` 保真映射(session.rs 注入解析器
/// 复用;severity 透传 + 根因链挂 `source`,零吞错)。
pub(crate) fn map_credential_error(err: partiverse_core::error::PartisyError) -> EngineError {
    EngineError::new(
        EngineErrorKind::ConfigInjectionUnresolved(err.to_string()),
        err.severity,
    )
    .with_source(Box::new(err))
}
