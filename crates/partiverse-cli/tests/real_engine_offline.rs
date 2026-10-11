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
    // create-123:123 形态(伪 .invalid https 端点过 123 校验,config 只存不连,
    // 零真实网络;T04 拆分后 123 专属校验面归 create-123)。
    connection::create_123(dispatch, &webdav_name, ENDPOINT, "13800000000", APP_PASS)
        .expect("create-123 ok");

    // ls 全链:operations/list(fs+remote 两键齐传)列沙箱工作区,T03 渲染面:
    // 表格输出(表头+条目行,非裸 JSON 起始),fixture 命中。
    let listing = browse::ls(dispatch, &format!("{local_name}:")).expect("ls ok");
    assert!(
        !listing.starts_with("{\""),
        "ls output must be a rendered table, not raw json: {listing}"
    );
    assert!(
        listing
            .lines()
            .next()
            .is_some_and(|header| header.starts_with("NAME")),
        "ls output must carry the table header: {listing}"
    );
    assert!(
        listing.lines().any(|line| line.contains("hello.txt")),
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

// —— M1-WP07-T02 DoD⑤:真引擎离线全链(mkdir→copy --wait→jobs list --watch
// + 预算 Exhausted 停放→retry 复活;同库单源 = 卡内 ④)——

use partiverse_cli::fsops;
use partiverse_cli::jobs as cli_jobs;
use partiverse_core::budget::{BudgetScheduler, QuotaProfiles};
use partiverse_core::fsops::poll_with_progress;
use partiverse_core::jobs::{JobManager, JobStatus};

/// 预算档(src node cap=2;fixture 数值非 provider 事实):copy cost=1 →
/// Allow;copy cost=3 → 结构性 Exhausted;retry cost=1 → Allow 复活。经公共
/// `load_with_override` 装载(core `parse` 为 crate 内面)。
fn t02_profiles(work: &std::path::Path, src_name: &str) -> QuotaProfiles {
    let json = format!(
        r#"{{"version":1,"profiles":{{"{src_name}":{{"requests_per_window":2,"window_secs":1000,"source":"test fixture"}}}},"bwlimit":{{"rate":"off"}},"backoff_secs":60}}"#
    );
    let path = work.join("quota-profiles.json");
    fs::write(&path, json).expect("write quota profiles fixture");
    QuotaProfiles::load_with_override(&path).expect("fixture parses")
}

/// 轮询至终态(200ms 间隔,测试侧执行;core 零 sleep)。
fn poll_until_terminal(
    manager: &JobManager,
    dispatch: &impl partiverse_core::jobs::RcDispatch,
    id: &str,
) -> partiverse_core::jobs::JobRecord {
    loop {
        let (record, _) = poll_with_progress(manager, dispatch, id).expect("poll with progress");
        if matches!(record.status, JobStatus::Done | JobStatus::Error) {
            return record;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn mkdir_copy_wait_watch_exhausted_retry_full_chain_on_real_engine() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    require_preinstalled_engine();
    let (slot, work) = test_slot("t02");
    let (src_dir, dst_dir) = (work.join("src"), work.join("dst"));
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::create_dir_all(&dst_dir).expect("create dst dir");
    const FIXTURE: &[u8] = b"partiverse cli t02 transfer";
    fs::write(src_dir.join("t02.txt"), FIXTURE).expect("write fixture file");
    let src_name = format!("pv-t02-src-{}", std::process::id());
    let dst_name = format!("pv-t02-dst-{}", std::process::id());

    let mut session = Session::open_with_slot(&slot).expect("session opens (ensure ready)");
    let dispatch = session.dispatch();
    connection::create_local(dispatch, &src_name, &src_dir.to_string_lossy())
        .expect("create-local src ok");
    connection::create_local(dispatch, &dst_name, &dst_dir.to_string_lossy())
        .expect("create-local dst ok");

    // 同库单源(卡内 ④:jobs+budget 同一 db 文件;生产 = open_default 同语义)。
    let db = work.join("partiverse.db");
    let manager = JobManager::open_at(&db).expect("job manager opens");
    let budget =
        BudgetScheduler::open_at(&db, t02_profiles(&work, &src_name)).expect("budget opens");

    // ① mkdir(无 profile → Allow):目录真实落盘。
    let out =
        fsops::mkdir(&budget, dispatch, &format!("{dst_name}:incoming"), 1).expect("mkdir ok");
    assert!(out.contains("directory created"), "{out}");
    assert!(
        dst_dir.join("incoming").is_dir(),
        "engine mkdir must land on disk"
    );

    // ② copy --wait(gated_submit 全链)→ 回执 + 轮询至 done + 落盘。
    let out = fsops::transfer(
        &manager,
        &budget,
        dispatch,
        false,
        &format!("{src_name}:"),
        &format!("{dst_name}:"),
        None,
        1,
        true,
    )
    .expect("copy --wait ok");
    assert!(
        out.contains("job submitted") && out.contains("job done"),
        "{out}"
    );
    assert_eq!(
        fs::read(dst_dir.join("t02.txt")).expect("copied file readable"),
        FIXTURE,
        "tree copy must land the fixture"
    );

    // ③ jobs list --watch:全链终态安定 → 快照一次即返回,done 入镜。
    let out = cli_jobs::list(&manager, None, true).expect("jobs list --watch ok");
    assert!(out.contains("done"), "{out}");

    // ④ 预算 Exhausted 停放(cost=3 > 桶容 2):queued 无锚,引擎零触达。
    let out = fsops::transfer(
        &manager,
        &budget,
        dispatch,
        false,
        &format!("{src_name}:"),
        &format!("{dst_name}:"),
        None,
        3,
        false,
    )
    .expect("exhausted copy reports parked");
    assert!(out.contains("budget exhausted"), "{out}");
    let queued = manager
        .list_jobs(Some(JobStatus::Queued))
        .expect("list queued");
    assert_eq!(queued.len(), 1, "恰一枚停放行: {queued:?}");
    assert!(queued[0].engine_job_id.is_none(), "停放行零引擎锚");
    let parked_id = queued[0].id.clone();

    // ⑤ retry 复活(壳决策树 core 单源)→ 轮询至 done,内容再次落盘。
    let out = cli_jobs::retry(&manager, &budget, dispatch, &parked_id, None, 1, None, None)
        .expect("jobs retry ok");
    assert!(out.contains("revived"), "{out}");
    let record = poll_until_terminal(&manager, dispatch, &parked_id);
    assert_eq!(record.status, JobStatus::Done, "revived job: {record:?}");
    assert_eq!(
        fs::read(dst_dir.join("t02.txt")).expect("revived copy readable"),
        FIXTURE
    );

    // 清场:config/delete 零残留 + 优雅退出。
    connection::delete(dispatch, &src_name).expect("delete src ok");
    connection::delete(dispatch, &dst_name).expect("delete dst ok");
    session.shutdown().expect("graceful shutdown");
    let _ = fs::remove_dir_all(&work);
}

// —— M1-WP07-T04 DoD④:真引擎离线全链(通用 webdav http 回环 serve 建连 →
// config/get 回读 → ls 可达;123 校验 http 端点拒绝上浮)——

use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};

use serde_json::Value;

use partiverse_core::error::Severity;
use partiverse_core::jobs::RcDispatch;
use partiverse_core::pan123::{Pan123ErrorKind, kind_of};

/// rclone serve webdav 子进程守卫(Drop 兜底 kill+wait,测试失败零残留进程)。
struct ServeGuard(Child);

impl Drop for ServeGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// 占用一个空闲回环端口(绑定后立即释放;极小 TOCTOU 窗口属测试沙箱面,
/// 端口被占 = serve 起不来/连不上,显式失败零静默)。
fn free_loopback_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback port");
    listener.local_addr().expect("local addr").port()
}

/// 等待 serve 端口可连(上限 ~10s;超时显式失败,零静默)。
fn wait_serve_port(port: u16) {
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("rclone serve webdav did not open 127.0.0.1:{port} within 10s");
}

#[test]
fn generic_webdav_http_serve_create_readback_ls_full_chain_on_real_engine() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    require_preinstalled_engine();
    let (slot, work) = test_slot("t04");
    let serve_dir = work.join("serve");
    fs::create_dir_all(&serve_dir).expect("create serve dir");
    fs::write(serve_dir.join("t04.txt"), b"partiverse cli t04 webdav")
        .expect("write serve fixture file");

    // 本地回环 serve webdav(卡内 DoD④:http 端点合法;零真实网络)。
    const SERVE_USER: &str = "pv-t04-user";
    const SERVE_PASS: &str = "pv-t04-serve-pass";
    let port = free_loopback_port();
    let serve_child = Command::new(require_preinstalled_engine())
        .args([
            "serve",
            "webdav",
            serve_dir.to_string_lossy().as_ref(),
            "--addr",
            &format!("127.0.0.1:{port}"),
            "--user",
            SERVE_USER,
            "--pass",
            SERVE_PASS,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rclone serve webdav");
    let _serve = ServeGuard(serve_child);
    wait_serve_port(port);

    let name = format!("pv-cli-webdav-generic-{}", std::process::id());
    let mut session = Session::open_with_slot(&slot).expect("session opens (ensure ready)");
    let dispatch = session.dispatch();

    // ① 通用 webdav 建连(http 回环端点直注,零 123 校验)。
    connection::create_webdav(
        dispatch,
        &name,
        &format!("http://127.0.0.1:{port}"),
        SERVE_USER,
        SERVE_PASS,
    )
    .expect("generic webdav create ok");

    // ② config/get 回读:type/url/user 在册;pass 为 obscure 密文(≠明文)。
    let readback = dispatch
        .call("config/get", &serde_json::json!({ "name": name }))
        .expect("config/get ok");
    assert_eq!(
        readback.get("type").and_then(Value::as_str),
        Some("webdav"),
        "type readback: {readback:?}"
    );
    assert_eq!(
        readback.get("url").and_then(Value::as_str),
        Some(format!("http://127.0.0.1:{port}").as_str()),
        "url readback: {readback:?}"
    );
    assert_eq!(
        readback.get("user").and_then(Value::as_str),
        Some(SERVE_USER),
        "user readback: {readback:?}"
    );
    let stored_pass = readback.get("pass").and_then(Value::as_str);
    assert_ne!(
        stored_pass,
        Some(SERVE_PASS),
        "stored pass must be obscured, never plaintext"
    );

    // ③ ls 全链:operations/list 经引擎 webdav 后端打到回环 serve,fixture 命中。
    let listing = browse::ls(dispatch, &format!("{name}:")).expect("ls over webdav ok");
    assert!(
        listing
            .lines()
            .next()
            .is_some_and(|header| header.starts_with("NAME")),
        "ls output must carry the table header: {listing}"
    );
    assert!(
        listing.lines().any(|line| line.contains("t04.txt")),
        "serve fixture file must be listed over webdav: {listing}"
    );

    // ④ connection list:连接在册;零参数值输出(凭据红线)。
    let connections = connection::list(dispatch).expect("connection list ok");
    assert!(
        connections
            .lines()
            .any(|line| line == format!("{name}\twebdav")),
        "generic webdav connection listed: {connections}"
    );
    assert!(
        !connections.contains(SERVE_PASS) && !connections.contains(SERVE_USER),
        "connection list output must never carry parameter values"
    );

    // 清场:config/delete 零残留 + 优雅退出;serve 由 Drop 守卫回收。
    connection::delete(dispatch, &name).expect("delete webdav ok");
    session.shutdown().expect("graceful shutdown");
    let _ = fs::remove_dir_all(&work);
}

#[test]
fn create_123_rejects_http_endpoint_with_invalid_123pan_error_on_real_engine() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    require_preinstalled_engine();
    let (slot, work) = test_slot("t04-reject");

    let name = format!("pv-cli-123-reject-{}", std::process::id());
    const HTTP_ENDPOINT: &str = "http://127.0.0.1:9";
    const ACCOUNT: &str = "13800000000";
    const PASS: &str = "t04-reject-pass";

    let mut session = Session::open_with_slot(&slot).expect("session opens (ensure ready)");
    let dispatch = session.dispatch();

    // 123 校验原样迁移不放宽:http 端点 → invalid 123pan 前置拒绝上浮。
    let err = connection::create_123(dispatch, &name, HTTP_ENDPOINT, ACCOUNT, PASS)
        .expect_err("http endpoint must be rejected by 123 validation");
    assert_eq!(err.severity, Severity::Fatal, "rejection severity: {err:?}");
    assert!(
        matches!(kind_of(&err), Some(Pan123ErrorKind::Invalid(_))),
        "structured invalid kind: {err:?}"
    );
    let text = err.to_string();
    assert!(
        text.contains("invalid 123pan webdav input"),
        "invalid 123pan text must surface: {text}"
    );
    // 载荷零凭据(错误面脱敏红线)。
    assert!(
        !text.contains(PASS) && !text.contains(ACCOUNT) && !text.contains(HTTP_ENDPOINT),
        "rejection text must be credential-free: {text}"
    );

    // 校验前置拒绝 = config 零残留(config/get 回读空对象,T01 删除语义锚)。
    let readback = dispatch
        .call("config/get", &serde_json::json!({ "name": name }))
        .expect("config/get ok");
    assert_eq!(
        readback,
        Value::Object(serde_json::Map::new()),
        "rejected create must leave no config residue: {readback:?}"
    );

    session.shutdown().expect("graceful shutdown");
    let _ = fs::remove_dir_all(&work);
}
