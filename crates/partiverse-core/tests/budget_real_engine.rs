//! M1-WP03-T03 卡内 DoD ⑥ 集成测试(真实 rclone 1.75.1,离线):字节限速下沉链
//! 配置→core/bwlimit——下发回显(rate/bytesPerSecond)与无参查询当前态一致,证
//! 「下发生效」;恢复 off 收尾。预算调度时序属纯逻辑,由假时钟单测覆盖。
// unix socket 门:引擎协调器与 rc 通道仅 unix(架构 §3 禁 TCP,同 T01/T02 集成口径)。
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use partiverse_core::budget::{QuotaProfiles, bwlimit};
use partiverse_core::error::PartisyError;
use partiverse_core::jobs::RcDispatch;
use partiverse_engine::{
    DEFAULT_SLOT_ID, EngineCoordinator, EngineInstaller, EngineManifest, EngineSlotConfig,
    RcClient, RcMethod,
};
use serde_json::Value;

/// 集成测试进程内串行(引擎安装根共享路径,同 T01/T02 集成纪律)。
fn test_serial_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// T01 RcClient → core RcDispatch 适配器(生产消费面形态:白名单收口+severity 保真;
/// core/bwlimit 在 T01 白名单内;白名单外本地拒绝路径已由 T01/T02 测试证明)。
struct RcAdapter {
    client: RcClient,
}

impl RcDispatch for RcAdapter {
    fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError> {
        let method = RcMethod::try_from_name(method)
            .map_err(|err| PartisyError::with_source(err.severity(), Box::new(err)))?;
        self.client
            .call(method, params)
            .map_err(|err| PartisyError::with_source(err.severity(), Box::new(err)))
    }
}

/// 预装引擎预检(防误触网络下载;缺失 = 显式失败+安装指引,禁静默 skip)。
fn require_preinstalled_engine() -> PathBuf {
    let root = EngineInstaller::resolve_install_root().expect("engine install root resolvable");
    let manifest = EngineManifest::embedded().expect("embedded manifest parses");
    let asset = manifest.current_platform().expect("host platform asset");
    let binary = root.join(&manifest.version).join(&asset.binary_name);
    assert!(binary.is_file(), "rclone missing; run engine-install");
    binary
}

#[test]
fn bwlimit_sinks_configured_rate_and_takes_effect_on_real_engine() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    require_preinstalled_engine();
    let work = std::env::temp_dir().join(format!("partiverse-budget-{}", std::process::id()));
    let mut slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("default slot loads");
    slot.cache_dir = work.join("cache");
    fs::create_dir_all(&slot.cache_dir).expect("create test cache dir");
    // 用户覆盖文件(卡内 ①:限速参数出自配置非 .rs 硬编码;fixture 速率非事实)。
    let override_path = work.join("quota-profiles.json");
    const OVERRIDE_JSON: &str = r#"{"version":1,"bwlimit":{"rate":"1Mi","source":"integration fixture (policy, not fact)"}}"#;
    fs::write(&override_path, OVERRIDE_JSON).expect("write user override");
    let profiles = QuotaProfiles::load_with_override(&override_path).expect("override loads");
    assert_eq!(profiles.bwlimit_rate, "1Mi");
    assert_eq!(profiles.profiles["jianguoyun"].requests_per_window, 600);
    let mut coordinator = EngineCoordinator::new().expect("coordinator constructs");
    let handle = coordinator.ensure(&slot).expect("ensure engine");
    let adapter = RcAdapter {
        client: handle.rc_client.clone(),
    };
    // 下发:响应回显设置参数(本机 1.75.1 实测锚定:1Mi→1048576)。
    let applied = bwlimit(&adapter, Some(&profiles.bwlimit_rate)).expect("apply bwlimit");
    assert_eq!(applied.rate, "1Mi");
    assert_eq!(applied.bytes_per_second, 1_048_576);
    // 无参查询引擎当前态 = 同值 → 下发生效(非仅应答回显)。
    let current = bwlimit(&adapter, None).expect("query bwlimit");
    assert_eq!(current, applied);
    // 收尾恢复 off(-1 = 不限),引擎随 shutdown 终止。
    let restored = bwlimit(&adapter, Some("off")).expect("restore off");
    assert_eq!(restored.bytes_per_second, -1);
    coordinator.shutdown(DEFAULT_SLOT_ID).expect("shutdown");
    let _ = fs::remove_dir_all(&work);
}
