//! M1-WP07-T01 DoD⑤ 真引擎离线集成测试(真实 rclone 1.75.1,零真实网络:
//! 后端仅 alias/webdav 存储,config 只存不连)。纪律照 app 壳 T09(commands.rs
//! 真引擎测试同款):预装引擎预检防误触下载、进程内串行(引擎安装根共享)、
//! 测试 remote `config/delete` 清场零残留。
// unix socket 门:引擎协调器与 rc 通道仅 unix(架构 §3 禁 TCP,同壳集成口径)。
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use partiverse_cli::browse;
use partiverse_cli::connection;
use partiverse_cli::session::Session;
use partiverse_engine::{
    DEFAULT_SLOT_ID, EngineInstaller, EngineManifest, EngineSlotConfig, RcdState,
};

/// 集成测试进程内串行(引擎安装根共享路径,壳 T09 同款纪律)。
fn test_serial_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 预装引擎预检(防误触网络下载;缺失 → 显式失败+安装指引,壳 T09 同款)。
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

/// 真引擎沙箱:独立临时工作区 + 槽位缓存隔离(cache_dir 指向沙箱,零触碰
/// 用户真实缓存目录);返回 (槽位, 工作区)。
fn test_slot(tag: &str) -> (EngineSlotConfig, PathBuf) {
    let work =
        std::env::temp_dir().join(format!("partiverse-cli-t01-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work); // 上轮残留清理(测试沙箱路径,非生产)
    let mut slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("default slot loads");
    slot.cache_dir = work.join("cache");
    fs::create_dir_all(&slot.cache_dir).expect("create test cache dir");
    (slot, work)
}

#[test]
fn session_lifecycle_reaches_ready_probes_and_shuts_down() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    require_preinstalled_engine();
    let (slot, work) = test_slot("lifecycle");
    let mut session = Session::open_with_slot(&slot).expect("session opens (ensure ready)");
    // 句柄观测面:ensure 返回时恒 Ready(T02 状态机),socket 路径非空。
    assert_eq!(session.handle().state, RcdState::Ready);
    // core/version 探活(按次启动会话内)。
    let version = session.probe().expect("core/version probe");
    assert!(version.starts_with('v'), "core version shape: {version}");
    session.shutdown().expect("graceful shutdown");
    let _ = fs::remove_dir_all(&work);
}

#[test]
fn create_local_create_webdav_ls_and_list_readback_on_real_engine() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    require_preinstalled_engine();
    let (slot, work) = test_slot("chain");
    fs::write(work.join("hello.txt"), b"partiverse cli t01").expect("write fixture file");

    // 测试名带 pid:防同进程历史残留互撞;测试收尾 config/delete 清场零残留。
    let local_name = format!("pv-cli-local-{}", std::process::id());
    let webdav_name = format!("pv-cli-webdav-{}", std::process::id());
    const ENDPOINT: &str = "https://webdav.example.invalid/webdav";
    const APP_PASS: &str = "t01-fixture-app-pass";

    let mut session = Session::open_with_slot(&slot).expect("session opens (ensure ready)");
    let dispatch = session.dispatch();

    // create-local:alias 后端包本地路径(卡内 ②;零硬编码,路径=沙箱工作区)。
    connection::create_local(dispatch, &local_name, &work.to_string_lossy())
        .expect("create-local ok");
    // create-webdav:123 形态(伪 .invalid 端点,config 只存不连,零真实网络)。
    connection::create_webdav(dispatch, &webdav_name, ENDPOINT, "13800000000", APP_PASS)
        .expect("create-webdav ok");

    // ls 全链:operations/list(fs+remote 两键齐传)列沙箱工作区,fixture 命中。
    let listing = browse::ls(dispatch, &format!("{local_name}:")).expect("ls ok");
    let reply: serde_json::Value = serde_json::from_str(&listing).expect("ls reply is json");
    let entries = reply
        .get("list")
        .and_then(serde_json::Value::as_array)
        .expect("ls reply carries list array");
    assert!(
        entries.iter().any(
            |entry| entry.get("Name").and_then(serde_json::Value::as_str) == Some("hello.txt")
        ),
        "fixture file must be listed: {listing}"
    );

    // connection list 回读断言:两连接在册且 type 正确;零参数值输出(凭据红线)。
    let connections = connection::list(dispatch).expect("connection list ok");
    assert!(
        connections
            .lines()
            .any(|line| line == format!("{local_name}\talias")),
        "local connection listed: {connections}"
    );
    assert!(
        connections
            .lines()
            .any(|line| line == format!("{webdav_name}\twebdav")),
        "webdav connection listed: {connections}"
    );
    assert!(
        !connections.contains(APP_PASS),
        "connection list output must never carry parameter values"
    );

    // 清场:config/delete + 回读断言零残留(connection::delete 内建回读)。
    connection::delete(dispatch, &local_name).expect("delete local ok");
    connection::delete(dispatch, &webdav_name).expect("delete webdav ok");

    session.shutdown().expect("graceful shutdown");
    let _ = fs::remove_dir_all(&work);
}
