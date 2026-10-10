//! 连接凭据管理(M1-WP04-T01):rclone config 加密主密钥的生成/keychain 托管/
//! 轮换,加密 config 的供给与可解密性校验,及日志脱敏过滤器。
//!
//! 零手写密码学:加密/解密全委托 rclone 1.75.1 config 加密机制(nacl
//! secretbox),密钥托管全委托 OS keychain(keyring 2.3.3:linux=内核 keyring /
//! windows=Credential Manager / macos=Keychain);本模块零加密/哈希/派生代码。
//!
//! rclone 机制出处(2026-10-10 本机 1.75.1 实测+内嵌官方文档核实):①
//! `config encryption set` 从 stdin 读两行 NEW/Confirm 密码,密文头
//! `RCLONE_ENCRYPT_V0:`;②`config encryption check` 退出码 0=可解密 /
//! 1=解密失败 / 2=未加密;③`RCLONE_CONFIG` env 选配置路径(DEBUG 日志
//! 实测);④`--password-command` 的 env 映射 `RCLONE_PASSWORD_COMMAND`
//! (注入后解密生效实测)。取密命令被 rclone 以 SpaceSepList 直接 exec
//! (不经 shell,实测),故生产取密面=可执行脚本(config/ 模板)。密钥只经
//! 子进程 stdin 或取密脚本传递,零 argv、零落盘、零日志;`redact` 为日志
//! 路径脱敏过滤器。

use std::fmt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{PartisyError, Severity, classify_io};

/// keychain 服务名(非机密,可入日志)。
pub const KEYRING_SERVICE: &str = "partiverse";
/// keychain 账户名(rclone config 主密钥专用条目;非机密,可入日志)。
pub const KEYRING_ACCOUNT: &str = "rclone-config";

/// rclone 官方 env 映射(1.75.1 实机核实):`--config` → `RCLONE_CONFIG`;
/// `--password-command` → `RCLONE_PASSWORD_COMMAND`(注入后解密生效实测)。
pub(crate) const RCLONE_CONFIG_ENV: &str = "RCLONE_CONFIG";
pub(crate) const RCLONE_PASSWORD_COMMAND_ENV: &str = "RCLONE_PASSWORD_COMMAND";

/// 结构化错误类别(severity 机器可判定,零吞错;除 Io 走核心 `classify_io`
/// 表驱动外均 Fatal:随机源缺失/键缺失/存储与解密失败皆重试无意义)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialErrorKind {
    /// CSPRNG 不可用(密钥缺失不可降级,Fatal);载荷=失败点描述。
    RandomSourceUnavailable(String),
    /// keychain 无主密钥条目(首次启用前/重启后内核 keyring 清空;ensure 内部
    /// 消化为生成触发,外部见到 = 需重新供给,Fatal)。
    KeyMissing,
    /// keychain 暂不可访问(store 锁定等,Retryable);载荷=上游错误文本。
    StorageUnavailable(String),
    /// keychain 平台层失败(权限/损坏等,Fatal);载荷=上游错误文本。
    PlatformFailure(String),
    /// 输入或上游返回不合法(非 UTF-8/超限/歧义/空名等,Fatal);载荷=描述。
    Invalid(String),
    /// 加密 config 无法以给定密钥解密(check 退出码 1;含轮换后旧 config
    /// 失效的显式上浮,Fatal);载荷=诊断文本(零密钥)。
    ConfigDecryptFailed(String),
    /// config 未加密(check 退出码 2,与预期不符,Fatal);载荷=诊断文本。
    ConfigNotEncrypted(String),
    /// 启用加密/供给 config 失败(子进程非零退出,Fatal);载荷=诊断文本。
    ConfigProvisionFailed(String),
    /// 文件系统/进程 IO(severity 由核心 `classify_io` 表驱动)。
    Io,
}

impl fmt::Display for CredentialErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CredentialErrorKind::RandomSourceUnavailable(d) => {
                write!(f, "random source unavailable: {d}")
            }
            CredentialErrorKind::KeyMissing => write!(f, "master key absent from keychain"),
            CredentialErrorKind::StorageUnavailable(d) => {
                write!(f, "keychain temporarily unavailable: {d}")
            }
            CredentialErrorKind::PlatformFailure(d) => {
                write!(f, "keychain platform failure: {d}")
            }
            CredentialErrorKind::Invalid(d) => write!(f, "invalid credential input: {d}"),
            CredentialErrorKind::ConfigDecryptFailed(d) => {
                write!(f, "encrypted config not decryptable: {d}")
            }
            CredentialErrorKind::ConfigNotEncrypted(d) => write!(f, "config is not encrypted: {d}"),
            CredentialErrorKind::ConfigProvisionFailed(d) => {
                write!(f, "config encryption provisioning failed: {d}")
            }
            CredentialErrorKind::Io => write!(f, "io error"),
        }
    }
}

impl std::error::Error for CredentialErrorKind {}

impl CredentialErrorKind {
    /// Fatal 构造(kind 挂 [`PartisyError::source`];jobs 同库复用模式)。
    pub(crate) fn fatal(self) -> PartisyError {
        PartisyError {
            severity: Severity::Fatal,
            source: Some(Box::new(self)),
        }
    }

    /// Retryable 构造(仅 StorageUnavailable 使用)。
    pub(crate) fn retryable(self) -> PartisyError {
        PartisyError {
            severity: Severity::Retryable,
            source: Some(Box::new(self)),
        }
    }
}

/// 从 [`PartisyError`] 还原结构化类别(非本模块来源的错误 → None)。
#[must_use]
pub fn kind_of(err: &PartisyError) -> Option<&CredentialErrorKind> {
    err.source
        .as_ref()
        .and_then(|s| s.downcast_ref::<CredentialErrorKind>())
}

/// IO 错误构造(severity 由核心 `classify_io` 表驱动)。
pub(crate) fn io_err(err: std::io::Error) -> PartisyError {
    PartisyError {
        severity: classify_io(err.kind()),
        source: Some(Box::new(err)),
    }
}

/// keychain 托管抽象(卡内 trait 接缝;测试用 `FakeKeyring`,生产为 [`OsKeyring`])。
pub trait KeyringStore: fmt::Debug {
    /// 写入/覆盖;失败按类上浮(存储不可访问 Retryable,其余 Fatal)。
    fn set(&self, secret: &str) -> Result<(), PartisyError>;
    /// 读取;缺失 → 结构化 `KeyMissing`(Fatal)。
    fn get(&self) -> Result<String, PartisyError>;
    /// 删除(幂等:缺失亦算成功)。
    fn delete(&self) -> Result<(), PartisyError>;
}

/// 生产 keychain 实现:keyring 2.3.3 原生存储薄包装。凭据命名(keyring 源码
/// 核实):linux keyutils description=`keyring-rs:{account}@{service}`
/// (keyutils.rs:251,KeyType::user);windows Credential Manager target=
/// `{account}.{service}`(windows.rs:289,UTF-16LE blob);macos Keychain
/// kSecAttrService=service / kSecAttrAccount=account(macos.rs:57)。内核
/// keyring 不跨重启(上游「secure cache」),重启后 `get` 得 `KeyMissing`
/// 由上层重新供给——选型已声明语义(ADR-0007)。
#[derive(Debug)]
pub struct OsKeyring {
    entry: keyring::Entry,
}

impl OsKeyring {
    /// 生产条目(固定服务/账户常量)。
    ///
    /// # Errors
    /// 平台存储不可用/参数非法时按类上浮。
    pub fn new() -> Result<Self, PartisyError> {
        Self::with_names(KEYRING_SERVICE, KEYRING_ACCOUNT)
    }

    /// 显式服务/账户条目(测试隔离面);空名 → 结构化 `Invalid`(Fatal)。
    ///
    /// # Errors
    /// 平台存储不可用/参数非法时按类上浮。
    pub fn with_names(service: &str, account: &str) -> Result<Self, PartisyError> {
        if service.is_empty() || account.is_empty() {
            return Err(CredentialErrorKind::Invalid(
                "keyring service/account must not be empty".into(),
            )
            .fatal());
        }
        let entry = keyring::Entry::new(service, account).map_err(map_keyring_error)?;
        Ok(OsKeyring { entry })
    }
}

impl KeyringStore for OsKeyring {
    fn set(&self, secret: &str) -> Result<(), PartisyError> {
        self.entry.set_password(secret).map_err(map_keyring_error)
    }

    fn get(&self) -> Result<String, PartisyError> {
        self.entry.get_password().map_err(map_keyring_error)
    }

    fn delete(&self) -> Result<(), PartisyError> {
        // NoEntry = 目标态已达成(幂等删除),非错误。
        match self.entry.delete_password() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(map_keyring_error(err)),
        }
    }
}

/// keyring 2.3.3 错误 → 结构化类别映射(error.rs 枚举对照源码)。
fn map_keyring_error(err: keyring::Error) -> PartisyError {
    match err {
        keyring::Error::NoEntry => CredentialErrorKind::KeyMissing.fatal(),
        keyring::Error::NoStorageAccess(source) => {
            CredentialErrorKind::StorageUnavailable(source.to_string()).retryable()
        }
        keyring::Error::PlatformFailure(source) => {
            CredentialErrorKind::PlatformFailure(source.to_string()).fatal()
        }
        // BadEncoding/TooLong/Invalid/Ambiguous 及非 exhaustive 通配(未知变体
        // 以原文归入 Invalid,零猜测归类)。
        _ => CredentialErrorKind::Invalid(err.to_string()).fatal(),
    }
}

/// 生成随机主密钥:32 字节 CSPRNG → 64 字符小写 hex(256 bit 熵;`[0-9a-f]`
/// 对 shell/env/stdin 全安全)。随机源按 ADR-0005 先例口径:unix=std 读
/// /dev/urandom(与 getrandom(2) 同源内核 CSPRNG);其余平台=getrandom 0.2
/// crate(BCryptGenRandom/getentropy)。
///
/// # Errors
/// 随机源不可用 → `RandomSourceUnavailable`(Fatal)。
pub fn generate_master_key() -> Result<String, PartisyError> {
    let bytes = random_bytes()?;
    Ok(bytes.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    }))
}

/// 读 32 字节 CSPRNG(平台分支,零手写密码学、零 unsafe)。
fn random_bytes() -> Result<[u8; 32], PartisyError> {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut buffer = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut buffer))
            .map_err(|err| {
                CredentialErrorKind::RandomSourceUnavailable(format!("read /dev/urandom: {err}"))
                    .fatal()
            })?;
        Ok(buffer)
    }
    #[cfg(not(unix))]
    {
        let mut buffer = [0u8; 32];
        getrandom::getrandom(&mut buffer).map_err(|err| {
            CredentialErrorKind::RandomSourceUnavailable(format!("platform csprng: {err}")).fatal()
        })?;
        Ok(buffer)
    }
}

/// 确保主密钥就位(读回命中 → 复用;`KeyMissing` → 生成+写+读回验证;写后
/// 读回 = 存储真实落位校验,不一致 Fatal,零静默)。
///
/// # Errors
/// keychain 访问失败按类上浮;生成失败 → `RandomSourceUnavailable`。
pub fn ensure_master_key(store: &impl KeyringStore) -> Result<String, PartisyError> {
    match store.get() {
        Ok(key) => Ok(key),
        Err(err) => match kind_of(&err) {
            Some(CredentialErrorKind::KeyMissing) => {
                let key = generate_master_key()?;
                store.set(&key)?;
                verify_stored(store, &key)?;
                Ok(key)
            }
            _ => Err(err),
        },
    }
}

/// 轮换主密钥(卡内语义:删除重生成,旧加密 config 随即失效;旧 config 的
/// 不可解密性由 [`verify_config_decryptable`] 以 `ConfigDecryptFailed` 显式
/// 上浮)。幂等容忍「本来就没有旧键」。
///
/// # Errors
/// keychain 访问失败按类上浮;生成失败 → `RandomSourceUnavailable`。
pub fn rotate_master_key(store: &impl KeyringStore) -> Result<String, PartisyError> {
    store.delete()?;
    let key = generate_master_key()?;
    store.set(&key)?;
    verify_stored(store, &key)?;
    Ok(key)
}

/// 写后读回验证(不一致 = 存储层异常,Fatal)。
fn verify_stored(store: &impl KeyringStore, expected: &str) -> Result<(), PartisyError> {
    if store.get()? == expected {
        Ok(())
    } else {
        Err(CredentialErrorKind::Invalid("keychain read-back mismatch after write".into()).fatal())
    }
}

/// 启用 rclone config 加密(模块头注 ①):config 文件先行创建,主密钥两行经
/// **stdin** 交付子进程(argv/env/磁盘零密钥)。
///
/// # Errors
/// 子进程非零退出 → `ConfigProvisionFailed`;IO 按核心表驱动。
pub fn enable_config_encryption(
    rclone: &Path,
    config_path: &Path,
    master_key: &str,
) -> Result<(), PartisyError> {
    if let Some(parent) = config_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    std::fs::File::create(config_path).map_err(io_err)?;
    let mut child = encryption_set_command(rclone, config_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io_err)?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            CredentialErrorKind::ConfigProvisionFailed("child stdin unavailable".into()).fatal()
        })?;
        // 两行 = Enter NEW / Confirm NEW(实测交互序列);写完即关 stdin。
        stdin.write_all(master_key.as_bytes()).map_err(io_err)?;
        stdin.write_all(b"\n").map_err(io_err)?;
        stdin.write_all(master_key.as_bytes()).map_err(io_err)?;
        stdin.write_all(b"\n").map_err(io_err)?;
    }
    let output = child.wait_with_output().map_err(io_err)?;
    if output.status.success() {
        return Ok(());
    }
    Err(CredentialErrorKind::ConfigProvisionFailed(format!(
        "rclone config encryption set failed ({}): {}",
        output.status,
        first_line(&output.stderr).unwrap_or_else(|| "no diagnostic output".into())
    ))
    .fatal())
}

/// 校验加密 config 可解密(模块头注 ②④):`RCLONE_PASSWORD_COMMAND` 注入
/// 取密脚本,退出码 0/1/2 判定;stdin 置空使交互提示立即 EOF 失败而非挂起
/// (实测)。生产 `password_command` = config/ 平台取密脚本(内容零密钥)。
///
/// # Errors
/// 退出码 1 → `ConfigDecryptFailed`(轮换失效/密码错显式上浮);2 →
/// `ConfigNotEncrypted`;其他非零 → `ConfigProvisionFailed`;IO 按表驱动。
pub fn verify_config_decryptable(
    rclone: &Path,
    config_path: &Path,
    password_command: &str,
) -> Result<(), PartisyError> {
    let output = encryption_check_command(rclone, config_path, password_command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(io_err)?;
    let detail = first_line(&output.stderr)
        .or_else(|| first_line(&output.stdout))
        .unwrap_or_else(|| "no diagnostic output".into());
    match output.status.code() {
        Some(0) => Ok(()),
        Some(1) => Err(CredentialErrorKind::ConfigDecryptFailed(detail).fatal()),
        Some(2) => Err(CredentialErrorKind::ConfigNotEncrypted(detail).fatal()),
        other => Err(CredentialErrorKind::ConfigProvisionFailed(format!(
            "unexpected rclone exit {other:?}: {detail}"
        ))
        .fatal()),
    }
}

/// 构造 `rclone config encryption set` 命令(argv 仅子命令;config 路径走
/// env;密钥由调用方经 stdin 交付)。独立函数供零密钥审计断言复用。
pub(crate) fn encryption_set_command(rclone: &Path, config_path: &Path) -> Command {
    let mut command = Command::new(rclone);
    command
        .arg("config")
        .arg("encryption")
        .arg("set")
        .env(RCLONE_CONFIG_ENV, config_path);
    command
}

/// 构造 `rclone config encryption check` 命令(argv 仅子命令;config 路径与
/// 取密脚本走 env)。独立函数供零密钥审计断言复用。
pub(crate) fn encryption_check_command(
    rclone: &Path,
    config_path: &Path,
    password_command: &str,
) -> Command {
    let mut command = Command::new(rclone);
    command
        .arg("config")
        .arg("encryption")
        .arg("check")
        .env(RCLONE_CONFIG_ENV, config_path)
        .env(RCLONE_PASSWORD_COMMAND_ENV, password_command);
    command
}

/// 日志脱敏过滤器:文本中出现的密钥值替换为 `<redacted>`;空串/单字符不
/// 替换(防空替换刷屏)。
#[must_use]
pub fn redact(text: &str, secret: &str) -> String {
    if secret.chars().count() < 2 {
        return text.to_owned();
    }
    text.replace(secret, "<redacted>")
}

/// 多行诊断首行(截断 200 字符;rcd.rs 同款防刷屏口径)。
fn first_line(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试替身(命名 Fake*,卡内边界):内存 keychain。
    #[derive(Debug, Default)]
    struct FakeKeyring {
        inner: std::sync::Mutex<Option<String>>,
    }

    impl FakeKeyring {
        fn lock(&self) -> Result<std::sync::MutexGuard<'_, Option<String>>, PartisyError> {
            self.inner.lock().map_err(|poisoned| {
                CredentialErrorKind::PlatformFailure(format!(
                    "fake store mutex poisoned: {poisoned}"
                ))
                .fatal()
            })
        }
    }

    impl KeyringStore for FakeKeyring {
        fn set(&self, secret: &str) -> Result<(), PartisyError> {
            *self.lock()? = Some(secret.to_owned());
            Ok(())
        }

        fn get(&self) -> Result<String, PartisyError> {
            match &*self.lock()? {
                Some(secret) => Ok(secret.clone()),
                None => Err(CredentialErrorKind::KeyMissing.fatal()),
            }
        }

        fn delete(&self) -> Result<(), PartisyError> {
            *self.lock()? = None;
            Ok(())
        }
    }

    #[test]
    fn generated_master_key_is_hex256_and_nondeterministic() {
        // 卡内 ①:密钥 = 64 hex(32 字节 CSPRNG);两次生成必不同。
        let first = generate_master_key().expect("key generates");
        let second = generate_master_key().expect("key generates");
        assert_eq!(first.len(), 64);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(first, second);
    }

    #[test]
    fn key_lifecycle_ensure_reuse_rotate_and_key_missing_is_structured() {
        // 卡内 ④:缺键 = 结构化 KeyMissing/Fatal;首调 ensure(缺键)→ 生成+
        // 写+读回,再调 → 复用同一键;轮换 = 删除+重生成(「旧 config 失效」
        // 显式错误面由 verify_config_decryptable 上浮,真 rclone 集成见
        // tests/),无旧键时同样成立(幂等)。
        let store = FakeKeyring::default();
        let missing = store.get().expect_err("no key yet");
        assert_eq!(missing.severity, Severity::Fatal);
        assert!(matches!(
            kind_of(&missing),
            Some(CredentialErrorKind::KeyMissing)
        ));
        let first = ensure_master_key(&store).expect("ensure creates");
        assert_eq!(store.get().expect("stored"), first);
        assert_eq!(ensure_master_key(&store).expect("ensure reuses"), first);
        let new = rotate_master_key(&store).expect("rotation");
        assert_ne!(first, new);
        assert_eq!(store.get().expect("stored"), new);
        assert_ne!(rotate_master_key(&store).expect("again"), new);
    }

    #[test]
    fn redact_filter_removes_every_occurrence() {
        // 卡内 ⑤:keychain 值不进日志的脱敏过滤器行为锚定。
        let secret = "deadbeefdeadbeef";
        let line = format!("config password derived via {secret} (len 16)");
        assert_eq!(
            redact(&line, secret),
            "config password derived via <redacted> (len 16)"
        );
        assert_eq!(
            redact("a→b a→b", "a"),
            "a→b a→b",
            "单字符密钥不替换(防空替换)"
        );
        assert_eq!(redact("untouched", ""), "untouched", "空密钥透传原文");
    }

    #[test]
    fn rclone_commands_carry_zero_secret_in_args_or_env() {
        // 卡内 ⑤:进程参数零密钥审计——set 命令 argv 仅子命令、env 仅 config
        // 路径;check 命令 env 的取密面=脚本路径,不含密钥本体。
        let key = generate_master_key().expect("key");
        let rclone = Path::new("/usr/bin/rclone");
        let config = Path::new("/tmp/pv-audit/rclone.conf");
        let set = encryption_set_command(rclone, config);
        assert_eq!(
            set.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["config", "encryption", "set"]
        );
        assert_eq!(set.get_envs().count(), 1, "set 命令仅允许 config 路径 env");
        let script = "/usr/local/bin/rclone-password-command.sh";
        let check = encryption_check_command(rclone, config, script);
        assert_eq!(
            check
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["config", "encryption", "check"]
        );
        assert!(check.get_envs().all(|(_, v)| {
            v.map(|v| !v.to_string_lossy().contains(key.as_str()))
                .unwrap_or(true)
        }));
    }
}
