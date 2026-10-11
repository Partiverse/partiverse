//! 连接凭据管理(M1-WP04-T01):rclone config 加密主密钥的生成/keychain 托管/
//! 轮换,加密 config 的供给与可解密性校验,及日志脱敏过滤器。
//!
//! M1-WP99-T03 追加(Owner 选型 2026-10-11,AskUserQuestion 四选一裁定):生产
//! 密钥托管 = **密钥文件 0600**(`~/.config/partiverse/master.key`,V1 不参数化;
//! windows=%APPDATA%\partiverse\master.key),不走 keychain(ADR-0007 内核
//! keyring 不跨重启的问题被绕开;`OsKeyring`/keychain 模块保留不删,后续裁定)。
//! 引擎启动的 config 注入决策([`decide_config_injection`])Fail-closed:
//! 加密 config + 密钥文件缺失 = 显式 Fatal,禁止降级明文启动。
//!
//! 零手写密码学:加密/解密全委托 rclone 1.75.1 config 加密机制(nacl
//! secretbox),keychain 托管委托 OS keychain(keyring 2.3.3:linux=内核 keyring /
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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{PartisyError, Severity, classify_io};

/// keychain 服务名(非机密,可入日志)。
pub const KEYRING_SERVICE: &str = "partiverse";
/// keychain 账户名(rclone config 主密钥专用条目;非机密,可入日志)。
pub const KEYRING_ACCOUNT: &str = "rclone-config";
/// 主密钥文件名(M1-WP99-T03,Owner 选型:密钥文件 0600;非机密,可入日志)。
pub const MASTER_KEY_FILE_NAME: &str = "master.key";
/// partiverse 平台配置子目录名(主密钥文件所在;slots 用户配置同侧惯例)。
const CONFIG_SUBDIR: &str = "partiverse";
/// rclone 加密 config 密文标记(M1-WP04-T01 实锚:enable 后文件含此行;实机
/// 2026-10-12 锚定密文首行为注释 `# Encrypted rclone configuration File`,
/// 标记在注释块**之后**而非文件前缀,探测须用 contains 而非 starts_with)。
pub const RCLONE_ENCRYPT_MARKER: &str = "RCLONE_ENCRYPT_V0:";

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
    /// 主密钥文件缺失(加密 config 在场而无 `master.key` = 引擎不可解密,
    /// Fail-closed 禁降级明文,Fatal;M1-WP99-T03);载荷=密钥文件路径(非机密)。
    MasterKeyFileMissing(String),
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
            // 卡内 ④ 原文短语(DoD:显式 Fatal「master key missing: config
            // undecryptable」);路径非机密可入错误面。
            CredentialErrorKind::MasterKeyFileMissing(path) => write!(
                f,
                "master key missing: config undecryptable (master key file: {path})"
            ),
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

/// 读 32 字节 CSPRNG(平台分支,零手写密码学、零 unsafe;crate 内复用面:
/// oauth::generate_state 与本模块主密钥同源,M1-WP04-T02)。
pub(crate) fn random_bytes() -> Result<[u8; 32], PartisyError> {
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

// —— M1-WP99-T03:密钥文件托管(Owner 选型 0600)+ 引擎注入 Fail-closed 决策 ——

/// 平台用户配置目录(与 engine slots 解析同法、不引 dirs,ADR-0004):
/// linux/其他 unix=XDG_CONFIG_HOME(仅绝对路径,XDG 规范)→ $HOME/.config;
/// macOS=~/Library/Application Support;windows=%APPDATA%。
fn platform_config_dir() -> Option<PathBuf> {
    fn env_dir(key: &str) -> Option<PathBuf> {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
    #[cfg(target_os = "windows")]
    {
        env_dir("APPDATA")
    }
    #[cfg(target_os = "macos")]
    {
        env_dir("HOME").map(|home| home.join("Library").join("Application Support"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match env_dir("XDG_CONFIG_HOME") {
            Some(dir) if Path::new(&dir).is_absolute() => Some(dir),
            _ => env_dir("HOME").map(|home| home.join(".config")),
        }
    }
}

/// 主密钥文件平台路径(卡内边界:V1 不参数化):`<配置目录>/partiverse/`
/// [`MASTER_KEY_FILE_NAME`]——linux=`~/.config/partiverse/master.key`,
/// windows=`%APPDATA%\partiverse\master.key`。目录不可解析 = 环境问题,Fatal。
///
/// # Errors
/// 平台配置目录不可解析 → `Invalid`(Fatal)。
pub fn master_key_file_path() -> Result<PathBuf, PartisyError> {
    platform_config_dir()
        .map(|dir| dir.join(CONFIG_SUBDIR).join(MASTER_KEY_FILE_NAME))
        .ok_or_else(|| {
            CredentialErrorKind::Invalid(
                "platform config dir unresolvable; cannot derive master key file path".into(),
            )
            .fatal()
        })
}

/// 主密钥文件保障结果(路径非机密;**密钥本体零入内**,可安全 Debug/日志)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterKeyFileStatus {
    /// 密钥文件绝对路径。
    pub path: PathBuf,
    /// 本次是否新建( false = 已存在复用)。
    pub created: bool,
    /// 既有文件权限非 0600、已自动修复(unix;windows ACL 面不适用恒 false;
    /// 调用方须据此出警告,卡内 ①「权限不对自动 chmod 0600+警告」)。
    pub permission_repaired: bool,
}

/// 确保主密钥文件就位(卡内 ①):缺失 → [`generate_master_key`] 生成 + 0600
/// 原子创建(`create_new` 拒绝竞态覆盖)+ 写后读回校验(T01 `ensure_master_key`
/// 同款纪律);已存在 → 复用 + unix 权限非 0600 自动修复。父目录新建按 0700
/// (unix;windows 继承用户 profile ACL,0600 语义=ACL 最小,实测归 Owner)。
/// 返回 (状态, 密钥本体)——密钥仅供调用方经 stdin 交付
/// [`enable_config_encryption`],禁日志/argv(`String` 不入 Debug 面)。
///
/// # Errors
/// 目录/文件 IO 按核心表驱动;内容形状非法 → `Invalid`(Fatal)。
pub fn ensure_master_key_file() -> Result<(MasterKeyFileStatus, String), PartisyError> {
    let path = master_key_file_path()?;
    let parent = path.parent().ok_or_else(|| {
        CredentialErrorKind::Invalid("master key path has no parent directory".into()).fatal()
    })?;
    #[cfg(unix)]
    {
        use std::fs::DirBuilder;
        use std::os::unix::fs::DirBuilderExt;
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(parent)
            .map_err(io_err)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    match std::fs::metadata(&path) {
        Ok(meta) => {
            let mut permission_repaired = false;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o777 != 0o600 {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .map_err(io_err)?;
                    permission_repaired = true;
                }
            }
            let key = read_master_key_file(&path)?;
            Ok((
                MasterKeyFileStatus {
                    path,
                    created: false,
                    permission_repaired,
                },
                key,
            ))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let key = generate_master_key()?;
            write_master_key_file(&path, &key)?;
            Ok((
                MasterKeyFileStatus {
                    path,
                    created: true,
                    permission_repaired: false,
                },
                key,
            ))
        }
        Err(err) => Err(io_err(err)),
    }
}

/// 读主密钥文件并校验内容形状(64 位小写 hex = [`generate_master_key`] 唯一
/// 产出形状):损坏/篡改显式 Fatal,零猜测继续(Fail-closed:错误密钥流入
/// rclone 只会产出费解的解密失败)。
///
/// # Errors
/// IO 按核心表驱动;形状不符 → `Invalid`(Fatal)。
pub fn read_master_key_file(path: &Path) -> Result<String, PartisyError> {
    let raw = std::fs::read_to_string(path).map_err(io_err)?;
    let key = raw.trim();
    let shape_ok = key.len() == 64
        && key
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    if !shape_ok {
        return Err(CredentialErrorKind::Invalid(format!(
            "master key file content invalid (expected 64 lowercase hex chars): {}",
            path.display()
        ))
        .fatal());
    }
    Ok(key.to_owned())
}

/// 0600 原子新建 + 写入 + 读回校验(`create_new` 拒绝覆盖竞态;尾换行为 unix
/// 惯例,rclone 取密输出容忍——1.75.1 实测带/不带尾换行 check 均 0)。
fn write_master_key_file(path: &Path, key: &str) -> Result<(), PartisyError> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(io_err)?;
        file.write_all(key.as_bytes()).map_err(io_err)?;
        file.write_all(b"\n").map_err(io_err)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, format!("{key}\n")).map_err(io_err)?;
    }
    // 写后读回 = 存储真实落位校验(不一致 Fatal,零静默)。
    if read_master_key_file(path)? != key {
        return Err(CredentialErrorKind::Invalid(
            "master key file read-back mismatch after write".into(),
        )
        .fatal());
    }
    Ok(())
}

/// config 是否已加密(密文标记 [`RCLONE_ENCRYPT_MARKER`] 包含探测——标记在
/// 密文注释头之后而非文件前缀,实机 2026-10-12 锚定;文件缺失 = 未加密
/// `false`;探测零子进程、零密码学、密钥零经手)。
///
/// # Errors
/// 打开/读取失败按核心表驱动(缺失文件除外)。
pub fn config_is_encrypted(config_path: &Path) -> Result<bool, PartisyError> {
    let text = match std::fs::read_to_string(config_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(io_err(err)),
    };
    Ok(text.contains(RCLONE_ENCRYPT_MARKER))
}

/// 引擎启动取密命令形态:`cat "<密钥文件绝对路径>"`(rclone 官方语义:命令
/// 输出即 config 密码,argv 零密钥)。**双引号必须**:rclone 1.75.1 以
/// SpaceSepList 直接 exec 取密命令(不经 shell,模块头注实锚),裸路径遇空格
/// 劈裂失败(实机实测),引号路径整体保真(实机实测 exit 0)。
#[must_use]
pub fn master_key_password_command(key_path: &Path) -> String {
    format!("cat \"{}\"", key_path.display())
}

/// 引擎启动的 config 注入决策(卡内 ②④,Fail-closed):
/// config 明文/缺失 → [`ConfigInjectionDecision::Plaintext`](无注入启动,上层
/// 提示);已加密 + 密钥文件就位 → [`ConfigInjectionDecision::Inject`](`cat`
/// 取密命令,rclone 官方语义);已加密 + 密钥文件缺失 → `MasterKeyFileMissing`
/// Fatal(DoD ④ 原文短语,**禁止降级明文启动**)。
///
/// # Errors
/// config 探测 IO 按核心表驱动;密钥文件缺失 → `MasterKeyFileMissing`(Fatal);
/// 密钥路径不可解析 → `Invalid`(Fatal)。
pub fn decide_config_injection(
    config_path: &Path,
) -> Result<ConfigInjectionDecision, PartisyError> {
    if !config_is_encrypted(config_path)? {
        return Ok(ConfigInjectionDecision::Plaintext);
    }
    let key_path = master_key_file_path()?;
    if !key_path.is_file() {
        return Err(
            CredentialErrorKind::MasterKeyFileMissing(key_path.display().to_string()).fatal(),
        );
    }
    Ok(ConfigInjectionDecision::Inject {
        password_command: master_key_password_command(&key_path),
    })
}

/// [`decide_config_injection`] 的决策产物(可安全 Debug:仅路径与取密命令,
/// 零密钥本体)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigInjectionDecision {
    /// config 已加密:以 `password_command`(`cat "<密钥文件>"`)注入启动。
    Inject {
        /// 取密命令(→ `RCLONE_PASSWORD_COMMAND`;内容零密钥本体)。
        password_command: String,
    },
    /// config 明文/缺失:无注入原样启动(上层提示 `pv config encrypt`)。
    Plaintext,
}

impl ConfigInjectionDecision {
    /// Inject 形态的取密命令(Plaintext → `None`;审计/测试取用面)。
    #[must_use]
    pub fn password_command(&self) -> Option<&str> {
        match self {
            ConfigInjectionDecision::Inject { password_command } => Some(password_command),
            ConfigInjectionDecision::Plaintext => None,
        }
    }
}

/// 启用 rclone config 加密(模块头注 ①):config 文件先行创建,主密钥两行经
/// **stdin** 交付子进程(argv/env/磁盘零密钥)。
///
/// M1-WP99-T03 两处修订(实机锚定 2026-10-11,真 rclone 1.75.1):
/// ① **迁移保真**:原实现 `File::create` 会把既有明文 config 截断为空,再加密
/// 空配置——Owner 现有连接(my123)将在迁移中被毁(DoD ⑥ 明令迁移后
/// connection/ls 仍可用)。改为 `create(true)+truncate(false)`:缺失则建(T01
/// 首次启用面),已存在则原样保留,rclone 在既有内容上整体加密(实锚:加密后
/// 解密侧 `config dump` 原样取回 remotes)。② **已加密守卫**:对已加密 config
/// 再打 `encryption set` 会按「当前密码」交互序列消费 stdin(实测两行被当作
/// 错误旧密码重试至 EOF 失败),语义不明——前置显式拒绝(Fail-closed),幂等面
/// 归调用方(`config_is_encrypted` 探测 + CLI 幂等提示)。
///
/// # Errors
/// config 已加密 → `Invalid`(Fatal);子进程非零退出 → `ConfigProvisionFailed`;
/// IO 按核心表驱动。
pub fn enable_config_encryption(
    rclone: &Path,
    config_path: &Path,
    master_key: &str,
) -> Result<(), PartisyError> {
    if config_is_encrypted(config_path)? {
        return Err(CredentialErrorKind::Invalid("config is already encrypted".into()).fatal());
    }
    if let Some(parent) = config_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(config_path)
        .map_err(io_err)?;
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

    /// 测试专用:进程级 env 互斥锁(主密钥文件路径读 HOME/XDG_CONFIG_HOME;
    /// slots::test_env_lock 同款先例,防 setenv/getenv 并发竞态)。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// HOME+XDG_CONFIG_HOME 隔离到临时目录(原值保存,收尾经 [`restore_home`]
    /// 原样恢复);返回 (隔离根, 隔离配置目录)。
    fn isolate_home(
        tag: &str,
    ) -> (
        std::path::PathBuf,
        PathBuf,
        (Option<std::ffi::OsString>, Option<std::ffi::OsString>),
    ) {
        let work = std::env::temp_dir().join(format!(
            "partiverse-credential-store-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).expect("create isolated home");
        let saved = (
            std::env::var_os("HOME"),
            std::env::var_os("XDG_CONFIG_HOME"),
        );
        // SAFETY:ENV_LOCK 串行化本二进制的 env 读写,测试内无并行读者。
        unsafe {
            std::env::set_var("HOME", &work);
            std::env::set_var("XDG_CONFIG_HOME", work.join(".config"));
        }
        (work.clone(), work.join(".config"), saved)
    }

    /// 恢复 env 原值 + 清理隔离目录(测试收尾,零残留)。
    fn restore_home(work: &Path, saved: (Option<std::ffi::OsString>, Option<std::ffi::OsString>)) {
        // SAFETY:ENV_LOCK 串行化本二进制的 env 读写。
        unsafe {
            match saved.0 {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
            match saved.1 {
                Some(dir) => std::env::set_var("XDG_CONFIG_HOME", dir),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(work);
    }

    #[test]
    fn master_key_file_lifecycle_creates_reuses_repairs_and_validates_shape() {
        // 卡内 ①⑤:缺失 → 生成 + 0600 强制 + 读回一致;已存在 → 复用同键;
        // 权限漂移 → 自动修复 0600 并置警告位;内容损坏 → 显式 Fatal 零猜测。
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (work, config_dir, saved) = isolate_home("lifecycle");
        let expected_path = config_dir.join(CONFIG_SUBDIR).join(MASTER_KEY_FILE_NAME);

        let (status, key) = ensure_master_key_file().expect("first ensure creates");
        assert_eq!(status.path, expected_path, "固定路径(卡内边界不参数化)");
        assert!(status.created && !status.permission_repaired);
        assert_eq!(key.len(), 64, "generate_master_key 形状(64 hex)");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&expected_path)
                .expect("key file exists")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "新建密钥文件必须 0600");
        }

        let (reused, key2) = ensure_master_key_file().expect("second ensure reuses");
        assert!(!reused.created && !reused.permission_repaired, "复用面");
        assert_eq!(key, key2, "复用必须返回同一密钥");

        // 权限漂移修复(卡内 ①:权限不对自动 chmod 0600+警告位)。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&expected_path, std::fs::Permissions::from_mode(0o644))
                .expect("drift permissions");
            let (repaired, key3) = ensure_master_key_file().expect("repair ensure");
            assert!(
                repaired.permission_repaired && !repaired.created,
                "{repaired:?}"
            );
            assert_eq!(key, key3);
            let mode = std::fs::metadata(&expected_path)
                .expect("key file exists")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "修复后必须回到 0600");
        }

        // 内容损坏:形状校验显式拒绝(Fail-closed,零猜测继续)。
        std::fs::write(&expected_path, "not-a-key\n").expect("corrupt key file");
        let err = ensure_master_key_file().expect_err("corrupt content must fail");
        assert_eq!(err.severity, Severity::Fatal);
        assert!(matches!(
            kind_of(&err),
            Some(CredentialErrorKind::Invalid(_))
        ));

        restore_home(&work, saved);
    }

    #[test]
    fn config_probe_and_injection_decision_are_fail_closed() {
        // 卡内 ②④:明文/缺失 → Plaintext;已加密缺密钥文件 → Fatal(DoD ④
        // 原文短语);密钥就位 → Inject(cat "<path>" 引号形态,实锚 SpaceSepList)。
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (work, config_dir, saved) = isolate_home("decision");
        let pv_dir = config_dir.join(CONFIG_SUBDIR);
        std::fs::create_dir_all(&pv_dir).expect("create partiverse config dir");
        let config = pv_dir.join("rclone.conf");

        // config 缺失 / 明文:Plaintext(零注入启动面)。
        assert!(!config_is_encrypted(&config).expect("absent config is plaintext"));
        assert_eq!(
            decide_config_injection(&config).expect("absent decides plaintext"),
            ConfigInjectionDecision::Plaintext
        );
        std::fs::write(&config, "[my123]\ntype = webdav\n").expect("write plaintext config");
        assert!(!config_is_encrypted(&config).expect("plaintext probe"));
        assert_eq!(
            decide_config_injection(&config).expect("plaintext decides plaintext"),
            ConfigInjectionDecision::Plaintext
        );

        // 已加密 + 密钥文件缺失:Fatal,DoD ④ 原文短语在错误面。
        std::fs::write(&config, format!("{RCLONE_ENCRYPT_MARKER}ciphertext")).expect("encrypt-ish");
        assert!(config_is_encrypted(&config).expect("marker probe"));
        let missing = decide_config_injection(&config).expect_err("missing key must fail");
        assert_eq!(missing.severity, Severity::Fatal);
        assert!(matches!(
            kind_of(&missing),
            Some(CredentialErrorKind::MasterKeyFileMissing(_))
        ));
        assert!(
            missing
                .to_string()
                .contains("master key missing: config undecryptable"),
            "DoD ④ 原文短语必须可见: {missing}"
        );

        // 密钥文件就位:Inject,取密命令 = cat "<绝对路径>"(引号保真)。
        let (status, key) = ensure_master_key_file().expect("key file ensured");
        let decision = decide_config_injection(&config).expect("key present decides inject");
        assert_eq!(
            decision,
            ConfigInjectionDecision::Inject {
                password_command: master_key_password_command(&status.path)
            }
        );
        assert!(
            decision
                .password_command()
                .is_some_and(|cmd| cmd.starts_with("cat \"") && cmd.ends_with('"')),
            "引号形态(SpaceSepList 实锚): {decision:?}"
        );
        assert!(
            !decision
                .password_command()
                .is_some_and(|cmd| cmd.contains(&key)),
            "取密命令零密钥本体(仅路径)"
        );

        restore_home(&work, saved);
    }
}
