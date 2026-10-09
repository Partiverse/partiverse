//! 引擎槽位配置(M1-WP02-T03):`EngineSlotConfig` serde 模型 + 三级级联加载。
//!
//! 加载优先级(卡内 ①):env `PARTIVERSE_ENGINE_SLOTS_FILE`(整文件覆盖,显式
//! 指针指向缺失/不可读文件 = 配置错误 Fatal 上浮,禁静默回退)> 用户配置目录
//! `partiverse/engine-slots.json`(缺失 = 该层未配置,按级联语义回落内嵌默认)>
//! 随仓内嵌 `config/engine-slots.default.json`(单槽位 default,亦是测试素材)。
//! 布局口径(架构 v1.0 app_local_data 同侧,与 T01 安装根并列、不引 dirs):
//! engine_root 默认沿用 T01 `resolve_install_root`(env `PARTIVERSE_ENGINE_ROOT`
//! 可覆盖);cache_dir 默认 = 平台数据目录 `partiverse/cache/<slot_id>`。
//! `data_port`/`remote_namespace` 仅落参数面:数据面监听属 WP07、remote 创建属
//! WP03/WP04,本卡零实现(Q5:槽位参数化,多 rcd 预留 Pro 位)。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, EngineErrorKind};
use crate::installer::{EngineInstaller, platform_data_dir};
use crate::manifest::is_safe_path_component;

/// 槽位表文件路径的 env 覆盖(卡内 ① 第一优先级;值为槽位表 JSON 文件路径)。
pub const SLOT_FILE_ENV: &str = "PARTIVERSE_ENGINE_SLOTS_FILE";
/// 随仓内嵌默认槽位表(编译期内嵌;单槽位 default,机器无关字段以 null 占位)。
pub const EMBEDDED_DEFAULT_SLOTS_JSON: &str =
    include_str!("../../../config/engine-slots.default.json");
/// 默认单槽位 id(卡内 ②:Q5 单实例以 default 槽位运行)。
pub const DEFAULT_SLOT_ID: &str = "default";

/// 数据面端口预留值:0 = 未分配(本卡不监听,WP07 数据面接入时由配置指定)。
const RESERVED_DATA_PORT: u16 = 0;

/// 引擎槽位参数(卡内 ①;解析与平台默认值落定后的最终形态)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineSlotConfig {
    /// 槽位 id(安全路径组件:用作 cache 子目录名与协调器单实例索引)。
    pub id: String,
    /// remote 命名空间前缀(仅参数面:WP03/WP04 建 remote 时消费,本卡零使用)。
    pub remote_namespace: String,
    /// 数据面端口(仅参数化,本卡不监听;0=预留未分配,数据面属 WP07)。
    pub data_port: u16,
    /// rclone `--cache-dir` 注入路径(槽位缓存隔离;显式相对路径按 rcd cwd 解析)。
    pub cache_dir: PathBuf,
    /// 引擎安装根(T01 布局 `<root>/<version>/<binary>`)。
    pub engine_root: PathBuf,
}

impl EngineSlotConfig {
    /// 按三级级联(env 覆盖 > 用户配置目录文件 > 内嵌默认)加载指定 id 的槽位。
    pub fn load_default(id: &str) -> Result<Self, EngineError> {
        if let Some(raw_path) = std::env::var_os(SLOT_FILE_ENV).filter(|v| !v.is_empty()) {
            let path = PathBuf::from(raw_path);
            // 显式 env 指针不可用 = 配置错误(缺文件/无权限),零静默回退:
            // IO 类失败包为 Fatal SlotConfigInvalid(根因链保留),解析类原样上浮。
            let slots = load_slots_file(&path).map_err(|err| match err.kind() {
                EngineErrorKind::Io => EngineError::new(
                    EngineErrorKind::SlotConfigInvalid(format!(
                        "env slot file unusable: {}",
                        path.display()
                    )),
                    partiverse_core::error::Severity::Fatal,
                )
                .with_source(Box::new(err)),
                _ => err,
            })?;
            return pick_slot(&slots, id);
        }
        if let Some(path) = user_slots_file() {
            // 用户配置文件按存在性进入级联:缺失 = 该层未配置,按设计回落内嵌
            // 默认(非吞错);其余 IO 失败(权限等)按环境问题上浮。
            match fs::read_to_string(&path) {
                Ok(text) => return pick_slot(&load_slots_json(&text)?, id),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(EngineError::from_io(err)),
            }
        }
        pick_slot(&load_slots_json(EMBEDDED_DEFAULT_SLOTS_JSON)?, id)
    }

    /// 手工构造实例的运行时守卫(ensure 入口强制调用;与解析期校验同一强度)。
    pub fn validate(&self) -> Result<(), EngineError> {
        validate_slot_fields(&self.id, &self.remote_namespace)?;
        ensure_non_empty_path(&self.cache_dir, "cache_dir")?;
        ensure_non_empty_path(&self.engine_root, "engine_root")
    }
}

/// 槽位表文件原始形态(serde;`null`/缺省字段 = 加载期按平台默认解析落定,
/// 保证随仓默认文件机器无关)。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSlotsFile {
    slots: Vec<RawSlotConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSlotConfig {
    id: String,
    remote_namespace: String,
    data_port: Option<u16>,
    cache_dir: Option<PathBuf>,
    engine_root: Option<PathBuf>,
}

/// 从 JSON 文本解析槽位表并逐槽校验/落定默认值。
pub fn load_slots_json(json: &str) -> Result<Vec<EngineSlotConfig>, EngineError> {
    let fatal = |detail: String| {
        EngineError::new(
            EngineErrorKind::SlotConfigInvalid(detail),
            partiverse_core::error::Severity::Fatal,
        )
    };
    let file: RawSlotsFile = serde_json::from_str(json).map_err(|err| fatal(err.to_string()))?;
    if file.slots.is_empty() {
        return Err(fatal("slots must not be empty".into()));
    }
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::with_capacity(file.slots.len());
    for raw in file.slots {
        if !seen.insert(raw.id.clone()) {
            return Err(fatal(format!("duplicate slot id: {}", raw.id)));
        }
        resolved.push(resolve_raw(raw)?);
    }
    Ok(resolved)
}

/// 从文件路径解析槽位表(缺失/不可读按 `classify_io` 分级上浮)。
pub fn load_slots_file(path: &Path) -> Result<Vec<EngineSlotConfig>, EngineError> {
    let text = fs::read_to_string(path).map_err(EngineError::from_io)?;
    load_slots_json(&text)
}

/// 按槽位 id 取槽位;不在表中 = 配置数据问题,Fatal `SlotNotFound` 上浮。
fn pick_slot(slots: &[EngineSlotConfig], id: &str) -> Result<EngineSlotConfig, EngineError> {
    slots
        .iter()
        .find(|slot| slot.id == id)
        .cloned()
        .ok_or_else(|| {
            EngineError::new(
                EngineErrorKind::SlotNotFound(id.to_owned()),
                partiverse_core::error::Severity::Fatal,
            )
        })
}

/// 原始槽位 → 最终槽位:显式字段保真,`None` 字段按平台默认落定后统一校验。
fn resolve_raw(raw: RawSlotConfig) -> Result<EngineSlotConfig, EngineError> {
    validate_slot_fields(&raw.id, &raw.remote_namespace)?;
    if let Some(path) = &raw.cache_dir {
        ensure_non_empty_path(path, "cache_dir")?;
    }
    if let Some(path) = &raw.engine_root {
        ensure_non_empty_path(path, "engine_root")?;
    }
    let cache_dir = match raw.cache_dir {
        Some(path) => path,
        None => platform_cache_dir(&raw.id)?,
    };
    let engine_root = match raw.engine_root {
        Some(path) => path,
        // 引擎根默认与 T01 安装根单源(env `PARTIVERSE_ENGINE_ROOT` 可覆盖)。
        None => EngineInstaller::resolve_install_root()?,
    };
    Ok(EngineSlotConfig {
        id: raw.id,
        remote_namespace: raw.remote_namespace,
        data_port: raw.data_port.unwrap_or(RESERVED_DATA_PORT),
        cache_dir,
        engine_root,
    })
}

/// 槽位 cache_dir 平台默认(数据目录 `partiverse/cache/<slot_id>`,与 T01
/// engine/ 子树并列;数据目录不可解析 = 环境问题,Fatal)。
pub(crate) fn platform_cache_dir(slot_id: &str) -> Result<PathBuf, EngineError> {
    platform_data_dir()
        .map(|dir| dir.join("partiverse").join("cache").join(slot_id))
        .ok_or_else(|| {
            EngineError::new(
                EngineErrorKind::SlotConfigInvalid(
                    "platform data dir unresolvable; cannot derive default cache_dir".into(),
                ),
                partiverse_core::error::Severity::Fatal,
            )
        })
}

/// 槽位字段校验:id 与 remote_namespace 均须为安全路径组件(隐式拒绝分隔符/
/// 遍历形/remote 名中的 `:` 等)。
fn validate_slot_fields(id: &str, remote_namespace: &str) -> Result<(), EngineError> {
    let fatal = |detail: String| {
        EngineError::new(
            EngineErrorKind::SlotConfigInvalid(detail),
            partiverse_core::error::Severity::Fatal,
        )
    };
    if !is_safe_path_component(id) {
        return Err(fatal(format!("unsafe slot id: {id:?}")));
    }
    if !is_safe_path_component(remote_namespace) {
        return Err(fatal(format!(
            "unsafe remote_namespace: {remote_namespace:?}"
        )));
    }
    Ok(())
}

/// 显式路径非空校验(空串路径在下游产生费解错误,解析期即拦截)。
fn ensure_non_empty_path(path: &Path, field: &str) -> Result<(), EngineError> {
    if path.as_os_str().is_empty() {
        return Err(EngineError::new(
            EngineErrorKind::SlotConfigInvalid(format!("{field} must not be empty")),
            partiverse_core::error::Severity::Fatal,
        ));
    }
    Ok(())
}

/// 平台用户配置目录(与 installer 的数据目录同法、不引 dirs,ADR-0004):
/// linux=XDG_CONFIG_HOME(仅绝对路径,XDG 规范)→ $HOME/.config;
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

/// 用户配置目录槽位表路径(级联第二层;缺失 = 该层未配置)。
fn user_slots_file() -> Option<PathBuf> {
    platform_config_dir().map(|dir| dir.join("partiverse").join("engine-slots.json"))
}

/// 测试专用:进程级 env 互斥锁(级联加载与安装根解析都读 env,同一 lib 测试
/// 二进制内凡读写 `PARTIVERSE_*` 的测试必须持锁,防 setenv/getenv 并发竞态)。
#[cfg(test)]
pub(crate) mod test_env_lock {
    use std::sync::Mutex;
    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::EngineErrorKind as Kind;
    use crate::slots::test_env_lock::ENV_LOCK;

    /// 构造单槽位 JSON(id/namespace 定制;可选显式路径)。
    fn one_slot_json(id: &str, namespace: &str) -> String {
        format!(r#"{{"slots":[{{"id":"{id}","remote_namespace":"{namespace}"}}]}}"#)
    }

    #[test]
    fn embedded_default_ships_single_default_slot_with_platform_defaults() {
        // 卡内 ②:随仓默认文件 = 单槽位 default;null 字段落定平台默认
        // (engine_root 与 T01 安装根同源;cache_dir = 数据目录 cache/<id>)。
        // load_default 读 env(PARTIVERSE_*):持共享 env 锁防并发写竞态。
        let _guard = ENV_LOCK.lock().expect("env lock");
        let slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("default slot loads");
        assert_eq!(slot.id, "default");
        assert_eq!(slot.remote_namespace, "pv");
        assert_eq!(slot.data_port, RESERVED_DATA_PORT, "port 0 = reserved");
        assert!(
            slot.engine_root
                .ends_with(Path::new("partiverse").join("engine")),
            "engine_root default follows T01 install root: {}",
            slot.engine_root.display()
        );
        assert!(
            slot.cache_dir
                .ends_with(Path::new("partiverse").join("cache").join("default")),
            "cache_dir default: {}",
            slot.cache_dir.display()
        );
        slot.validate().expect("resolved slot passes validation");
    }

    #[test]
    fn explicit_fields_round_trip_through_serde() {
        // serde 模型保真:显式路径/data_port 原样解析;再序列化往返一致。
        let json = r#"{"slots":[{"id":"pro","remote_namespace":"pvp","data_port":45177,"cache_dir":"/tmp/pv-pro-cache","engine_root":"/opt/pv/engine"}]}"#;
        let slots = load_slots_json(json).expect("explicit slot parses");
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].id, "pro");
        assert_eq!(slots[0].remote_namespace, "pvp");
        assert_eq!(slots[0].data_port, 45177);
        assert_eq!(slots[0].cache_dir, PathBuf::from("/tmp/pv-pro-cache"));
        assert_eq!(slots[0].engine_root, PathBuf::from("/opt/pv/engine"));
        let round_trip = serde_json::to_string(&slots[0]).expect("serialize");
        let reparsed =
            load_slots_json(&format!(r#"{{"slots":[{round_trip}]}}"#)).expect("round trip parses");
        assert_eq!(reparsed[0], slots[0]);
    }

    #[test]
    fn invalid_slot_files_are_rejected_as_fatal() {
        // 表驱动:未知字段/空表/重复 id/遍历 id/非法 remote_namespace → 全 Fatal。
        let cases: Vec<(&str, String)> = vec![
            (
                "unknown field",
                r#"{"slots":[{"id":"a","remote_namespace":"pv","extra":1}]}"#.into(),
            ),
            ("empty slots", r#"{"slots":[]}"#.into()),
            (
                "duplicate ids",
                format!(
                    r#"{{"slots":[{},{}]}}"#,
                    one_slot_json("a", "pv"),
                    one_slot_json("a", "pv2")
                ),
            ),
            ("traversal id", one_slot_json("../evil", "pv")),
            ("colon namespace", one_slot_json("a", "pv:evil")),
        ];
        for (label, json) in cases {
            let err = load_slots_json(&json).map(|_| ()).expect_err(label);
            assert_eq!(
                err.severity(),
                partiverse_core::error::Severity::Fatal,
                "{label}"
            );
            assert!(
                matches!(err.kind(), Kind::SlotConfigInvalid(_)),
                "{label}: {:?}",
                err.kind()
            );
        }
    }

    #[test]
    fn env_file_overrides_cascade_and_missing_env_file_is_fatal() {
        // 卡内 ① 第一优先级:env 指针文件 > 其余层;env 指向缺失文件 = Fatal
        // (零静默回退);请求未配置 id = Fatal SlotNotFound。
        let _guard = ENV_LOCK.lock().expect("env lock");
        let dir = std::env::temp_dir().join(format!("partiverse-slots-env-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let env_file = dir.join("slots.json");
        fs::write(&env_file, one_slot_json("default", "envns")).expect("write env slot file");
        // SAFETY:测试内 ENV_LOCK 串行化本二进制的 env 读写。
        unsafe {
            std::env::set_var(SLOT_FILE_ENV, &env_file);
        }
        let slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("env tier wins");
        assert_eq!(slot.remote_namespace, "envns");
        let err = EngineSlotConfig::load_default("no-such-slot").expect_err("id not in table");
        assert!(matches!(err.kind(), Kind::SlotNotFound(_)));
        // env 指向缺失文件:Fatal,不静默回落内嵌默认。
        let missing = dir.join("missing.json");
        unsafe {
            std::env::set_var(SLOT_FILE_ENV, &missing);
        }
        let err = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect_err("missing env file");
        assert_eq!(err.severity(), partiverse_core::error::Severity::Fatal);
        assert!(matches!(err.kind(), Kind::SlotConfigInvalid(_)));
        // SAFETY:收尾恢复环境。
        unsafe {
            std::env::remove_var(SLOT_FILE_ENV);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_default_picks_requested_slot_or_fails_by_id() {
        // 层级无关断言(env 未设时落在用户文件层或内嵌默认层,均须成立):
        // 请求存在的 id → Ok;请求不存在的 id → Fatal SlotNotFound。
        let _guard = ENV_LOCK.lock().expect("env lock");
        // SAFETY:测试内 ENV_LOCK 串行化本二进制的 env 读写。
        unsafe {
            std::env::remove_var(SLOT_FILE_ENV);
        }
        EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("default slot resolvable");
        let err = EngineSlotConfig::load_default("no-such-slot").expect_err("id absent");
        assert!(matches!(err.kind(), Kind::SlotNotFound(_)));
    }
}
