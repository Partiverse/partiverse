//! M1-WP06-T02 卡内 DoD ①② 集成测试(真实 rclone 1.75.1,离线):rc 形状逐件
//! 实测锚定(禁凭记忆)——`operations/mkdir`(嵌套段)、`operations/delete`(目录
//! 树文件删除/文件路径 Fatal)、单文件过滤器 sync/copy(srcFs 指向文件必失败,
//! 父目录+`_filter.FilterRule` 才是单文件语义)、`operations/copyfile`(dstRemote
//! 重命名)、`core/stats {group:"job/<id>"}` 进度采样落 T01 progress 列。
// unix socket 门:引擎协调器与 rc 通道仅 unix(架构 §3 禁 TCP,同 T01 集成口径)。
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use partiverse_core::budget::{BudgetScheduler, GatedSubmit, QuotaProfiles};
use partiverse_core::caps::ProviderCaps;
use partiverse_core::error::{PartisyError, Severity};
use partiverse_core::fsops::{
    TransferSpec, compose_fs, extract_progress, fs_delete, fs_mkdir, poll_with_progress,
    single_file_transfer, transfer_submit,
};
use partiverse_core::jobs::{JobManager, JobStatus, RcDispatch};
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

/// T01 RcClient → core RcDispatch 适配器(生产消费面形态证明)。
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

/// 预装引擎预检(防误触网络下载;缺失 → 显式失败+安装指引)。
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

fn make_workspace(binary: &Path) -> (EngineSlotConfig, PathBuf) {
    let work = std::env::temp_dir().join(format!("partiverse-fsops-{}", std::process::id()));
    let mut slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("default slot loads");
    slot.cache_dir = work.join("cache");
    fs::create_dir_all(&slot.cache_dir).expect("create test cache dir");
    for dir in ["src", "dst"] {
        fs::create_dir_all(work.join(dir)).expect("create data dir");
    }
    let status = Command::new(binary)
        .args(["test", "makefiles"])
        .arg(work.join("src"))
        .args([
            "--files",
            "3",
            "--flat",
            "--max-file-size",
            "1k",
            "--seed",
            "7",
        ])
        .status()
        .expect("spawn rclone test makefiles");
    assert!(status.success(), "rclone test makefiles failed");
    (slot, work)
}

fn create_alias(adapter: &RcAdapter, name: &str, path: &Path) {
    adapter
        .call(
            "config/create",
            &json!({"name": name, "type": "alias", "parameters": {"remote": path.to_string_lossy()}}),
        )
        .expect("config/create alias succeeds");
}

#[test]
fn fs_ops_shapes_single_file_filter_and_group_progress_on_real_engine() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    let binary = require_preinstalled_engine();
    let (slot, work) = make_workspace(&binary);
    let mut coordinator = EngineCoordinator::new().expect("coordinator constructs");
    let handle = coordinator.ensure(&slot).expect("ensure engine");
    let adapter = RcAdapter {
        client: handle.rc_client.clone(),
    };
    create_alias(&adapter, "fsrc", &work.join("src"));
    create_alias(&adapter, "fdst", &work.join("dst"));
    // 预算/任务同库(node 无 profile = 不限流;限流门已由单测覆盖)。
    let db = work.join("partiverse.db");
    let profiles = BudgetScheduler::open_at(
        &db,
        QuotaProfiles::builtin().expect("builtin profiles parse"),
    )
    .expect("open budget");
    let manager = JobManager::open_at(&db).expect("open job store");

    // ① mkdir:嵌套段一次建齐(实测双参数 {fs, remote} 形状),落盘对账。
    assert_eq!(
        fs_mkdir(&profiles, &adapter, "local", "fsrc:", "deep/nested", 1).expect("mkdir"),
        partiverse_core::fsops::FsOpOutcome::Applied
    );
    assert!(work.join("src/deep/nested").is_dir(), "nested mkdir landed");

    // ② delete(目录树文件删除,保留目录壳):造两个文件后删,壳仍在。
    fs::create_dir_all(work.join("src/wipe")).expect("seed wipe dir");
    fs::write(work.join("src/wipe/a.txt"), "a").expect("seed a");
    fs::write(work.join("src/wipe/b.txt"), "b").expect("seed b");
    assert_eq!(
        fs_delete(&profiles, &adapter, "local", "fsrc:wipe", 1).expect("delete"),
        partiverse_core::fsops::FsOpOutcome::Applied
    );
    assert!(
        work.join("src/wipe").is_dir(),
        "directory shell must remain"
    );
    assert!(
        fs::read_dir(work.join("src/wipe"))
            .expect("wipe dir")
            .next()
            .is_none(),
        "files under the tree must be gone"
    );
    // delete 指向文件 = Fatal "is a file not a directory"(1.75.1 实测锚定):
    // 取造数产物中的真实文件路径验证必败。
    let file_name = fs::read_dir(work.join("src"))
        .expect("list src")
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_file())
        .expect("a seeded file exists")
        .file_name();
    let err = fs_delete(
        &profiles,
        &adapter,
        "local",
        &compose_fs("fsrc:", &file_name.to_string_lossy()),
        1,
    )
    .expect_err("file fs must be rejected by operations/delete");
    // 直接 rc 调用的 severity 由引擎按 HTTP 500 判 Retryable(5xx 瞬态语义,
    // WP03 分级);锚定断言的是错误文本本身(job 级 Fatal 分类属 jobs 层职责)。
    assert_eq!(err.severity, Severity::Retryable);
    assert!(
        format!("{err}").contains("is a file not a directory"),
        "anchored engine text expected: {err}"
    );

    // ③ 单文件传输(无 caps 保守路径):srcFs=父目录 + FilterRule 单文件过滤。
    // 对照实证:srcFs 指向文件本身必失败(锚定 readdirent not a directory)。
    let direct = manager
        .submit(
            &adapter,
            "sync/copy",
            "copy",
            "fsrc:seeded-file",
            "fdst:",
            &json!({ "srcFs": compose_fs("fsrc:", &file_name.to_string_lossy()), "dstFs": "fdst:" }),
        )
        .expect("submit file-as-fs probe");
    let mut probe_err = String::new();
    for _ in 0..50 {
        let (record, _) = manager.poll(&adapter, &direct.id).expect("poll probe");
        if matches!(record.status, JobStatus::Done | JobStatus::Error) {
            probe_err = record.error.unwrap_or_default();
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        probe_err.contains("not a directory"),
        "file-as-srcFs must fail (anchored): {probe_err:?}"
    );
    let seeded = fs::read_dir(work.join("src"))
        .expect("list src")
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_file())
        .expect("seeded file")
        .file_name()
        .to_string_lossy()
        .to_string();
    let outcome = single_file_transfer(
        &manager,
        &profiles,
        &adapter,
        &partiverse_core::fsops::SingleFileSpec {
            node: "local",
            cost: 1,
            is_move: false,
            kind: "copy",
            src_fs_base: "fsrc:",
            src_remote: &seeded,
            dst_fs: "fdst:",
            dst_remote: &seeded,
            src_caps: None,
            dst_caps: None,
        },
    )
    .expect("single file filter copy");
    let partiverse_core::fsops::SingleFileOutcome::Submitted(job) = outcome else {
        panic!("expected Submitted on unprofiled node, got {outcome:?}")
    };
    let mut final_record = None;
    for _ in 0..100 {
        let (record, _) = manager.poll(&adapter, &job.id).expect("poll single");
        if matches!(record.status, JobStatus::Done | JobStatus::Error) {
            final_record = Some(record);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let record = final_record.expect("single-file copy done in 10s");
    assert!(
        record.error.is_none(),
        "filter copy must succeed: {:?}",
        record.error
    );
    assert!(
        work.join("dst").join(&seeded).is_file(),
        "file landed at dst"
    );

    // ④ 服务端单文件操作(双端 caps 协商):copyfile + dstRemote 重命名。
    // non_exhaustive 跨 crate 构造走官方 ::new 出口(caps.rs 契约)。
    let caps = || {
        Some(ProviderCaps::new(
            partiverse_core::caps::HashCaps::default(),
            partiverse_core::caps::MtimePrecision::None,
            false,
            true,
            false,
            false,
            false,
        ))
    };
    let renamed = "renamed-on-copy.txt";
    let applied = single_file_transfer(
        &manager,
        &profiles,
        &adapter,
        &partiverse_core::fsops::SingleFileSpec {
            node: "local",
            cost: 1,
            is_move: false,
            kind: "copy",
            src_fs_base: "fsrc:",
            src_remote: &seeded,
            dst_fs: "fdst:",
            dst_remote: renamed,
            src_caps: caps(),
            dst_caps: caps(),
        },
    )
    .expect("serverside copyfile");
    assert_eq!(
        applied,
        partiverse_core::fsops::SingleFileOutcome::SyncApplied
    );
    assert!(
        work.join("dst").join(renamed).is_file(),
        "renamed copy landed"
    );

    // ⑤ 进度聚合:大文件 sync/copy → poll_with_progress 采样 core/stats 组落列。
    let status = Command::new(&binary)
        .args(["test", "makefiles"])
        .arg(work.join("bigsrc"))
        .args([
            "--files",
            "1",
            "--flat",
            "--min-file-size",
            "32MiB",
            "--max-file-size",
            "32MiB",
            "--seed",
            "9",
        ])
        .status()
        .expect("spawn makefiles big");
    assert!(status.success());
    create_alias(&adapter, "fbig", &work.join("bigsrc"));
    let big_name = fs::read_dir(work.join("bigsrc"))
        .expect("list bigsrc")
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_file())
        .expect("big file")
        .file_name()
        .to_string_lossy()
        .to_string();
    let size = fs::metadata(work.join("bigsrc").join(&big_name))
        .expect("big metadata")
        .len();
    // 轮询至 done;running 轮由 poll_with_progress 采样落列(CI 噪声重试 ≤2 轮)。
    'outer: for _attempt in 0..2 {
        let GatedSubmit::Submitted(big_job) = transfer_submit(
            &manager,
            &profiles,
            &adapter,
            &TransferSpec {
                node: "local",
                cost: 1,
                is_move: false,
                kind: "copy",
                src_fs: &compose_fs("fbig:", ""),
                dst_fs: "fdst:bigout/",
            },
        )
        .expect("submit big") else {
            panic!("expected Submitted big job")
        };
        let mut saw_running = false;
        for _ in 0..300 {
            let (record, _) =
                poll_with_progress(&manager, &adapter, &big_job.id).expect("poll big");
            match record.status {
                JobStatus::Running => saw_running = true,
                JobStatus::Done => {
                    // 组采样须在终态前落列(真机实测):列必有真实引擎采样值
                    // (bytes/totalBytes 为引擎采样时刻值——首拍可在 listing 阶段
                    // = 0,中程可小于 size,如 30568448/33554432;精确时序不钉死,
                    // 采样机制本身由单测脚本化钉死)。
                    assert!(
                        record.progress_bytes.is_some() && record.progress_total.is_some(),
                        "progress 列须被真实组采样落库: {:?}",
                        (record.progress_bytes, record.progress_total)
                    );
                    assert!(
                        record.progress_bytes.is_some_and(|b| b <= size),
                        "bytes 采样不得超文件大小: {:?}",
                        record.progress_bytes
                    );
                    break 'outer;
                }
                JobStatus::Queued => {}
                JobStatus::Error => {
                    let err = record.error.unwrap_or_default();
                    assert!(!saw_running, "sampling observed but job errored: {err}");
                    continue 'outer; // 环境性失败重试链
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("big job not done within 15s");
    }
    assert!(
        work.join("dst/bigout").join(&big_name).is_file(),
        "big copy landed"
    );
    // extract_progress 对真实组载荷形状收口(未知组 = 全零对象,实测无错)。
    let empty = adapter
        .call("core/stats", &json!({ "group": "job/999999" }))
        .expect("stats unknown group");
    assert_eq!(
        extract_progress(&empty).expect("unknown group parses"),
        Some((0, Some(0)))
    );
}
