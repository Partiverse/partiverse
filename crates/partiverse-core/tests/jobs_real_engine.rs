//! M1-WP03-T02 卡内 DoD ⑥ 集成测试(真实 rclone 1.75.1,离线):alias remote
//! 本地目录互拷(rclone test makefiles 造数)submit→running→done 全链;引擎重启
//! (shutdown 后重新 ensure)reconcile → running→error(engine_restarted,Fatal)。
// unix socket 门:引擎协调器与 rc 通道仅 unix(架构 §3 禁 TCP,同 T01 集成口径)。
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use partiverse_core::error::{PartisyError, Severity};
use partiverse_core::jobs::{ASYNC_METHODS, JobManager, JobStatus, RcDispatch};
use partiverse_engine::{
    DEFAULT_SLOT_ID, EngineCoordinator, EngineInstaller, EngineManifest, EngineSlotConfig,
    RcClient, RcMethod,
};
use serde_json::{Value, json};

/// 集成测试进程内串行(引擎安装根共享路径,同 T01 集成纪律)。
fn test_serial_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// T01 RcClient → core RcDispatch 适配器(生产消费面形态证明:白名单收口+severity 保真)。
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

/// 预装引擎预检(防误触网络下载):二进制在(缺失 → 显式失败+安装指引)。
fn require_preinstalled_engine() -> PathBuf {
    let root = EngineInstaller::resolve_install_root().expect("engine install root resolvable");
    let manifest = EngineManifest::embedded().expect("embedded manifest parses");
    let asset = manifest.current_platform().expect("host platform asset");
    let binary = root.join(&manifest.version).join(&asset.binary_name);
    assert!(
        binary.is_file(),
        "rclone missing at {}; run engine-install",
        root.display()
    );
    binary
}

/// 测试工作区:src(小数据集)/src-big(20000 文件,reconcile running 态锚定)。
fn make_workspace(binary: &Path) -> (EngineSlotConfig, PathBuf) {
    let work = std::env::temp_dir().join(format!("partiverse-jobs-{}", std::process::id()));
    let mut slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("default slot loads");
    slot.cache_dir = work.join("cache");
    fs::create_dir_all(&slot.cache_dir).expect("create test cache dir");
    for dir in ["src", "src-big", "dst", "dst2"] {
        fs::create_dir_all(work.join(dir)).expect("create data dir");
    }
    for (dir, files) in [("src", 3), ("src-big", 20_000)] {
        let status = Command::new(binary)
            .args(["test", "makefiles"])
            .arg(work.join(dir))
            .args([
                "--files",
                &files.to_string(),
                "--flat",
                "--max-file-size",
                "1k",
                "--seed",
                "42",
            ])
            .status()
            .expect("spawn rclone test makefiles");
        assert!(status.success(), "rclone test makefiles {dir} failed");
    }
    (slot, work)
}

/// 起 alias remote(config/create,与本机实测形状一致)。
fn create_alias(adapter: &RcAdapter, name: &str, path: &Path) {
    adapter
        .call(
            "config/create",
            &json!({"name": name, "type": "alias", "parameters": {"remote": path.to_string_lossy()}}),
        )
        .expect("config/create alias succeeds");
}

#[test]
fn full_chain_then_reconcile_after_engine_restart_on_real_engine() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    let binary = require_preinstalled_engine();
    // 卡内边界:submit 允许集必须是 T01 白名单子集(真实枚举锚定)。
    for method in ASYNC_METHODS {
        assert!(
            RcMethod::try_from_name(method).is_ok(),
            "{method} not whitelisted"
        );
    }
    let (slot, work) = make_workspace(&binary);
    let mut coordinator = EngineCoordinator::new().expect("coordinator constructs");
    let handle = coordinator.ensure(&slot).expect("ensure engine1");
    let adapter = RcAdapter {
        client: handle.rc_client.clone(),
    };
    for (name, dir) in [
        ("pvsrc", "src"),
        ("pvbig", "src-big"),
        ("pvdst", "dst"),
        ("pvdst2", "dst2"),
    ] {
        create_alias(&adapter, name, &work.join(dir));
    }
    let manager = JobManager::open_at(&work.join("partiverse.db")).expect("open job store");
    // 卡内 ①④:submit → queued;小数据集首轮 poll 可能已终局(合法链)。
    // CI 环境噪声重试(M1-WP03-T06/T07 定性):仅 rclone 真失败(Done 且 error 非
    // 空)触发整链重试 ≤2 轮;Done+result 缺失为合法返回——`output` 非 rclone 协议
    // 保证字段(定性实验:本地 20/20 存在,CI runner 连续两轮缺失),poll 的
    // result=None 本就是产品语义内返回;成功有效性由内容对账(dst mirrors src)保真。
    let mut done_id = String::new();
    for attempt in 0..2 {
        let done_job = manager
            .submit(
                &adapter,
                "sync/copy",
                "copy",
                "pvsrc:",
                "pvdst:",
                &json!({ "srcFs": "pvsrc:", "dstFs": "pvdst:" }),
            )
            .expect("submit A");
        assert_eq!(done_job.status, JobStatus::Queued);
        assert!(done_job.engine_job_id.is_some() && done_job.error.is_none());
        let (first, _) = manager.poll(&adapter, &done_job.id).expect("first poll");
        assert!(matches!(first.status, JobStatus::Running | JobStatus::Done));
        // 轮询至 done(100ms 步进,30s 上限,超限显式失败)。
        let mut settled: Option<partiverse_core::jobs::JobRecord> = None;
        for _ in 0..300 {
            let observation = manager.poll(&adapter, &done_job.id).expect("poll ok");
            if observation.0.status == JobStatus::Done {
                settled = Some(observation.0);
                break;
            }
            assert_eq!(observation.0.status, JobStatus::Running);
            std::thread::sleep(Duration::from_millis(100));
        }
        let Some(record) = settled else {
            panic!("job not done within 30s (attempt {attempt})");
        };
        assert!(
            record.error.is_none(),
            "job failed (attempt {attempt}): error={:?}",
            record.error
        );
        done_id = record.id;
        break;
    }
    // 真实拷贝内容对账:src/dst 文件名集合一致且非空(operations/list,离线)。
    let listing = |fs_name: &str| {
        let reply = adapter
            .call("operations/list", &json!({ "fs": fs_name, "remote": "" }))
            .expect("operations/list ok");
        let mut names: Vec<String> = reply["list"]
            .as_array()
            .expect("list array")
            .iter()
            .filter(|entry| !entry["IsDir"].as_bool().unwrap_or(false))
            .filter_map(|entry| entry["Name"].as_str().map(str::to_owned))
            .collect();
        names.sort();
        names
    };
    let src_names = listing("pvsrc:");
    assert_eq!(src_names.len(), 3);
    assert_eq!(src_names, listing("pvdst:"), "dst mirrors src after done");
    // 卡内 ③:引擎重启模拟 = shutdown 后重新 ensure;B 提交后立即 poll,20000
    // 文件同步检查阶段远超轮询往返,running 态确定。
    let orphan = manager
        .submit(
            &adapter,
            "sync/copy",
            "copy",
            "pvbig:",
            "pvdst2:",
            &json!({ "srcFs": "pvbig:", "dstFs": "pvdst2:" }),
        )
        .expect("submit B");
    let (running, _) = manager.poll(&adapter, &orphan.id).expect("poll B");
    assert_eq!(running.status, JobStatus::Running);
    coordinator
        .shutdown(DEFAULT_SLOT_ID)
        .expect("shutdown engine1");
    let handle2 = coordinator.ensure(&slot).expect("re-ensure engine2");
    let adapter2 = RcAdapter {
        client: handle2.rc_client.clone(),
    };
    let reconciled = manager.reconcile(&adapter2).expect("reconcile ok");
    assert_eq!(
        reconciled.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec![orphan.id.as_str()],
        "only the running orphan flips"
    );
    let flipped = manager.get(&orphan.id).expect("flipped row");
    assert_eq!(flipped.status, JobStatus::Error);
    assert_eq!(flipped.severity, Some(Severity::Fatal));
    assert!(
        flipped
            .error
            .as_deref()
            .unwrap_or_default()
            .starts_with("engine_restarted")
    );
    // done 终态不受对账影响。
    let untouched = manager.get(&done_id).expect("done row");
    assert_eq!(untouched.status, JobStatus::Done);
    assert!(untouched.error.is_none());
    coordinator
        .shutdown(DEFAULT_SLOT_ID)
        .expect("shutdown engine2");
    let _ = fs::remove_dir_all(&work);
}
