//! M1-WP06-T04 集成测试(Fake 后端错误注入,离线):官方 rclone 1.75.1 无 baidu
//! 后端(oauth.rs:19-20 实机证据)→ 引擎侧 dlink 失败文本经 FakeScriptDispatch
//! 注入(job/status error 载荷),dlink 重取源为 Fake 实现(真实现随 backends-go/
//! WP11 落地)。覆盖:分类命中→重取→重建参数→回队→复活全链、逐轮余量耗尽、
//! 复活失败后的可观测停放与 T01 恢复路径、非 dlink 文本让位。真实面 = JobManager
//! SQLite 持久化 + T01 状态机/守卫全链。**端到端真机验证显式排除,挂 WP11;
//! 本套件不证明生产可用性(骨架,非可用功能)**。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use partiverse_core::dlink::{DlinkFailure, DlinkRefetch, DlinkRetryOutcome, dlink_refetch_retry};
use partiverse_core::error::{PartisyError, Severity};
use partiverse_core::jobs::{JobErrorKind, JobManager, JobRecord, JobStatus, RcDispatch};
use serde_json::{Value, json};

/// 进程内唯一临时库路径(用例间零串扰)。
fn workspace() -> (JobManager, PathBuf) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "partiverse-dlink-it-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    let manager = JobManager::open_at(&path)
        .expect("open job store")
        .with_retry_policy(partiverse_core::jobs::RetryPolicy {
            max_retries: 2,
            backoff: Duration::from_secs(60),
        });
    (manager, path)
}

/// Fake rc 分发(仅测试):脚本化依序应答并记录调用。
struct FakeScriptDispatch {
    replies: RefCell<VecDeque<Value>>,
    calls: RefCell<Vec<(String, Value)>>,
}

impl FakeScriptDispatch {
    fn scripted(replies: &[Value]) -> Self {
        Self {
            replies: RefCell::new(replies.iter().cloned().collect()),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl RcDispatch for FakeScriptDispatch {
    fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), params.clone()));
        self.replies.borrow_mut().pop_front().ok_or_else(|| {
            PartisyError::with_source(
                Severity::Fatal,
                Box::new(JobErrorKind::Invalid("script exhausted".into())),
            )
        })
    }
}

/// Fake dlink 重取后端(仅测试,Fake* 纪律):按脚本依序发新 dlink,记录 target
/// 调用(真实现 = baidu filemetas 族,baidu.md:39;挂 backends-go/WP11)。
struct FakeDlinkBackend {
    fresh: RefCell<VecDeque<&'static str>>,
    targets: RefCell<Vec<String>>,
}

impl FakeDlinkBackend {
    fn scripted(fresh: &[&'static str]) -> Self {
        Self {
            fresh: RefCell::new(fresh.iter().copied().collect()),
            targets: RefCell::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        self.targets.borrow().len()
    }
}

impl DlinkRefetch for FakeDlinkBackend {
    fn refetch_dlink(&self, target: &str) -> Result<String, PartisyError> {
        self.targets.borrow_mut().push(target.to_owned());
        self.fresh
            .borrow_mut()
            .pop_front()
            .map(str::to_owned)
            .ok_or_else(|| {
                PartisyError::with_source(
                    Severity::Fatal,
                    Box::new(JobErrorKind::Invalid("fake dlink script exhausted".into())),
                )
            })
    }
}

/// job/status 应答造数。
fn running() -> Value {
    json!({ "finished": false, "success": false, "error": "" })
}

fn failed(error: &str) -> Value {
    json!({ "finished": true, "success": false, "error": error, "output": {} })
}

/// 两段 poll 至失败落终态(每次 poll 消费一条脚本应答:带锚 queued 行首轮恒
/// running,须两次 poll)。
fn poll_to_failure(manager: &JobManager, id: &str, error: &str) {
    let _ = manager
        .poll(&FakeScriptDispatch::scripted(&[running()]), id)
        .expect("poll running");
    let _ = manager
        .poll(&FakeScriptDispatch::scripted(&[failed(error)]), id)
        .expect("poll failure");
}

/// 提交一枚 sync/copy job(脚本化引擎应答;params 携带旧 dlink)。
fn submit_job(manager: &JobManager, jobid: i64, old_dlink: &str) -> JobRecord {
    let dispatch = FakeScriptDispatch::scripted(&[json!({ "jobid": jobid })]);
    manager
        .submit(
            &dispatch,
            "sync/copy",
            "copy",
            old_dlink,
            "pvdst:",
            &json!({ "srcFs": old_dlink }),
        )
        .expect("submit")
}

#[test]
fn fake_backend_injected_dlink_failures_drive_full_refetch_chain() {
    let (manager, path) = workspace();
    let backend = FakeDlinkBackend::scripted(&["fresh-1", "fresh-2"]);
    let target = "baidu:/apps/partiverse/a.bin";
    let expired = "transfer failed: {\"error_code\":31360,\"errmsg\":\"dlink has expired\"}";
    let hotlink = "transfer failed: {\"error_code\":31326,\"errmsg\":\"hotlink protection\"}";
    let job = submit_job(&manager, 1, "baidu-dl:v0");
    let id = job.id;

    // 第 1 轮:引擎(Inject)失败文本 = 31360 → 通用分类保守 fatal 落终态。
    let _ = manager
        .poll(&FakeScriptDispatch::scripted(&[running()]), &id)
        .expect("poll running");
    let (record, _) = manager
        .poll(&FakeScriptDispatch::scripted(&[failed(expired)]), &id)
        .expect("poll expired");
    assert_eq!(
        (record.status, record.severity, record.retries),
        (JobStatus::Error, Some(Severity::Fatal), 0)
    );
    // 编排救援 1:重取(fresh-1)→ 重建 → 回队 → 复活(jobid=2)。
    let revive1 = FakeScriptDispatch::scripted(&[json!({ "jobid": 2 })]);
    let outcome = dlink_refetch_retry(
        &manager,
        &revive1,
        &backend,
        &id,
        target,
        "sync/copy",
        |fresh| Ok(json!({ "srcFs": format!("baidu-dl:{fresh}") })),
    )
    .expect("rescue 1");
    assert!(matches!(
        &outcome,
        DlinkRetryOutcome::RefetchedAndRevived {
            failure: DlinkFailure::DlinkExpired,
            ..
        }
    ));
    assert_eq!(backend.targets.borrow().as_slice(), [target]);
    let (method, params) = revive1.calls.borrow().last().expect("revive call").clone();
    assert_eq!(method, "sync/copy");
    assert_eq!(
        params,
        json!({ "srcFs": "baidu-dl:fresh-1", "_async": true })
    );

    // 第 2 轮:31326 防盗链文本 → HotlinkProtected → 救援 2(fresh-2,jobid=3)。
    poll_to_failure(&manager, &id, hotlink);
    let revive2 = FakeScriptDispatch::scripted(&[json!({ "jobid": 3 })]);
    let outcome = dlink_refetch_retry(
        &manager,
        &revive2,
        &backend,
        &id,
        target,
        "sync/copy",
        |fresh| Ok(json!({ "srcFs": format!("baidu-dl:{fresh}") })),
    )
    .expect("rescue 2");
    assert!(matches!(
        &outcome,
        DlinkRetryOutcome::RefetchedAndRevived {
            failure: DlinkFailure::HotlinkProtected,
            ..
        }
    ));
    let (method, params) = revive2
        .calls
        .borrow()
        .last()
        .expect("revive 2 call")
        .clone();
    assert_eq!(method, "sync/copy");
    assert_eq!(
        params,
        json!({ "srcFs": "baidu-dl:fresh-2", "_async": true })
    );
    assert_eq!(manager.get(&id).expect("record").retries, 2);

    // 第 3 轮:余量耗尽 → RetriesExhausted,重取/复活零触达,行零改动。
    poll_to_failure(&manager, &id, expired);
    let before = manager.get(&id).expect("record before");
    let probe = FakeScriptDispatch::scripted(&[]);
    let outcome = dlink_refetch_retry(
        &manager,
        &probe,
        &backend,
        &id,
        target,
        "sync/copy",
        |fresh| Ok(json!({ "srcFs": format!("baidu-dl:{fresh}") })),
    )
    .expect("rescue 3 declines");
    assert_eq!(
        outcome,
        DlinkRetryOutcome::RetriesExhausted {
            record: before.clone(),
            failure: DlinkFailure::DlinkExpired,
        }
    );
    assert_eq!(backend.call_count(), 2, "余量耗尽不重取");
    assert_eq!(probe.calls.borrow().len(), 0, "余量耗尽不复活");
    assert_eq!(manager.get(&id).expect("reread"), before, "行零改动");
    // 终态行经 list_jobs 可观测(error 文本保留末次失败)。
    let errors = manager
        .list_jobs(Some(JobStatus::Error))
        .expect("list error");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].id, id);
    assert_eq!(errors[0].retries, 2);
    assert!(
        errors[0]
            .error
            .as_deref()
            .expect("error text")
            .contains("31360")
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn revive_failure_leaves_observable_parked_row_recoverable_via_t01() {
    // 复活失败(引擎失联形态):回队已发生 → 行 = queued 无锚、retries+1、
    // severity=retryable、旧错误文本保留(零吞错,编排层可观测);恢复路径 =
    // T01 revive_parked(参数由调用方重建,新 dlink 注入)。
    let (manager, path) = workspace();
    let backend = FakeDlinkBackend::scripted(&["fresh-1"]);
    let expired = "transfer failed: {\"error_code\":31360,\"errmsg\":\"dlink has expired\"}";
    let job = submit_job(&manager, 11, "baidu-dl:v0");
    let id = job.id;
    let _ = manager
        .poll(&FakeScriptDispatch::scripted(&[running()]), &id)
        .expect("poll running");
    let _ = manager
        .poll(&FakeScriptDispatch::scripted(&[failed(expired)]), &id)
        .expect("poll failure");
    // 复活脚本为空 = 引擎提交失败 → 编排返回结构化错误,行停放在可复活态。
    let err = dlink_refetch_retry(
        &manager,
        &FakeScriptDispatch::scripted(&[]),
        &backend,
        &id,
        "t",
        "sync/copy",
        |fresh| Ok(json!({ "srcFs": format!("baidu-dl:{fresh}") })),
    )
    .expect_err("revive failure floats");
    assert_eq!(err.severity, Severity::Fatal);
    let parked = manager.get(&id).expect("parked row");
    assert_eq!(
        (parked.status, parked.engine_job_id, parked.retries),
        (JobStatus::Queued, None, 1),
        "回队已执行:清锚+计数"
    );
    assert_eq!(
        parked.severity,
        Some(Severity::Retryable),
        "dlink 分类改标保真"
    );
    assert!(
        parked
            .error
            .as_deref()
            .expect("kept text")
            .contains("31360"),
        "旧失败文本保留供观测"
    );
    // T01 恢复路径:编排层持新参数直接 revive_parked 成功(标注清空)。
    let dispatch = FakeScriptDispatch::scripted(&[json!({ "jobid": 12 })]);
    let revived = manager
        .revive_parked(
            &dispatch,
            &id,
            "sync/copy",
            &json!({ "srcFs": "baidu-dl:fresh-1" }),
        )
        .expect("t01 recovery");
    assert_eq!(
        (revived.status, revived.engine_job_id),
        (JobStatus::Queued, Some(12))
    );
    assert_eq!((revived.error.as_deref(), revived.severity), (None, None));
    assert_eq!(backend.call_count(), 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn non_dlink_failure_text_declines_to_generic_path() {
    // 未命中让位:NotDlinkFailure,重取零触达、行零改动(交通用/手动处置)。
    let (manager, path) = workspace();
    let backend = FakeDlinkBackend::scripted(&[]);
    let job = submit_job(&manager, 21, "baidu-dl:v0");
    let id = job.id;
    poll_to_failure(&manager, &id, "directory not found");
    let before = manager.get(&id).expect("record");
    let outcome = dlink_refetch_retry(
        &manager,
        &FakeScriptDispatch::scripted(&[]),
        &backend,
        &id,
        "t",
        "sync/copy",
        |_| Ok(json!({})),
    )
    .expect("decline");
    assert_eq!(
        outcome,
        DlinkRetryOutcome::NotDlinkFailure {
            record: before.clone()
        }
    );
    assert_eq!(backend.call_count(), 0);
    assert_eq!(manager.get(&id).expect("reread"), before);
    let _ = std::fs::remove_file(&path);
}
