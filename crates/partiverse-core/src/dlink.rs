//! dlink 失效重取编排骨架(M1-WP06-T04,31360 类;**端到端真机验证显式排除,挂 WP11**)。
//!
//! # 边界事实(卡面锚定,禁静默标绿)
//! 官方 rclone 1.75.1 无 baidu 后端(`rclone authorize baidu` → didn't find backend
//! called "baidu";`help backends` 无 baidu 系条目,oauth.rs:19-20 实机证据;
//! backends-go fork 未落地)→ filemetas 重取在官方 rc 面无对应命令。本模块 =
//! 错误分类 + 重取接口抽象([`DlinkRefetch`])+ 编排骨架;**生产可用性随 WP11
//! 真机验证与 backends-go 落地确认,本卡不把骨架标注为可用功能**;Fake 实现仅
//! 测试(集成测试经 Fake 错误注入覆盖编排逻辑),禁生产桩冒充交付。
//!
//! # provider 事实(docs/providers/baidu.md = 接入唯一事实源)
//! - 取 dlink = filemetas 族:`POST /xpan/file?method=filemetas`(路径数组)/
//!   `GET /xpan/multimedia?method=filemetas`(fsids ≤100,**取 dlink 用这个**)
//!   (baidu.md:39);
//! - 下载 = `GET <dlink>&access_token=...`,必须 Header `User-Agent: pan.baidu.com`
//!   (否则 31326 防盗链);dlink 时效过期 = 31360(baidu.md:40);
//! - 错误码速查:31360 dlink 过期 / 31326 防盗链(baidu.md:68);111 = 异步任务
//!   进行中(baidu.md:27/:44)——非 dlink 失效,不入本分类(注释登记防混淆);
//! - 卡内裁定:31360/31326 两类均 Retryable(修复面 = 重取新 dlink + 重建参数
//!   后重试,非同参盲重)。
//!
//! # 编排(卡内 ②,骨架)
//! 调用序(编排层):[`JobManager::poll`](T01)观测失败落 error 终态(错误文本
//! 零吞上浮)→ [`dlink_refetch_retry`]:分类命中 → [`DlinkRefetch::refetch_dlink`]
//! 重取 → 调用方 `rebuild` 闭包重建任务参数(core 零发明 baidu 参数面)→
//! [`JobManager::requeue_for_dlink_refetch`](crate 内专设守卫通道,error→queued、
//! retries+1;T03 `mark_verify_failed` 守卫通道先例,WP03 冻结 `can_transition`
//! 表不变)→ [`JobManager::revive_parked`](T01 API,method 白名单/queued 无锚
//! 窗口守卫复用)。重试上限沿 T01:行内 `retries` 对照 manager 配置的
//! [`RetryPolicy::max_retries`](计数与上限单源,编排器不自设第二套);重取/
//! 重建失败 severity 保真上浮,行保持 error 终态可再次救援。预算 re-acquire 与
//! 回队退避由编排层前置(T01 同口径,core 零 sleep)。
//!
//! # 已知边界(如实登记,禁静默)
//! - 分类规则 = 错误码裸串包含匹配的保守形态:引擎侧错误文本真实形状未经实机
//!   锚定(无 baidu 后端),粗粒度匹配可能撞无关数字串(见单测登记例);实机
//!   文本到位后随 WP11 收紧规则表,禁静默。
//! - 若失败文本同时命中 T01 实机 Retryable 签名与 dlink 码,通用链先行回队
//!   消费重试余量(同参盲重),终态后方由本编排接管;该双签名场景无实机样本,
//!   WP11 复核(queued 停放态的重取注入属编排层,不在本骨架)。

use serde_json::Value;

use crate::error::{PartisyError, Severity};
use crate::jobs::{JobErrorKind, JobManager, JobRecord, JobStatus, RcDispatch};

/// dlink 类失败结构化分类(卡内 ①;31360/31326 事实引用 baidu.md:68)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DlinkFailure {
    /// 31360:dlink 时效过期(baidu.md:40/:68)。
    DlinkExpired,
    /// 31326:防盗链(下载请求缺 `User-Agent: pan.baidu.com`,baidu.md:40/:68)。
    HotlinkProtected,
}

impl DlinkFailure {
    /// 触发错误码原文(观测/日志 key 用,英文)。
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::DlinkExpired => "31360",
            Self::HotlinkProtected => "31326",
        }
    }

    /// canonical 小写串(日志 key/呈现用,英文)。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DlinkExpired => "dlink_expired",
            Self::HotlinkProtected => "hotlink_protected",
        }
    }

    /// 卡内裁定:两类均 Retryable(自动重试通道准入;修复面 = 重取+重建,非同参盲重)。
    #[must_use]
    pub fn severity(self) -> Severity {
        Severity::Retryable
    }
}

/// 匹配规则表(卡内 ① 表驱动,首中即停):错误码裸串 → 结构化分类。事实锚定
/// baidu.md:68(31360 dlink 过期 / 31326 防盗链)。引擎侧错误文本真实形状未经
/// 实机锚定(官方 1.75.1 无 baidu 后端,oauth.rs:19-20)→ 现行规则为保守裸串
/// 包含匹配,实机文本到位后随 WP11 收紧(扩充/收紧均须附证据,禁凭记忆)。
const DLINK_RULES: &[(&str, DlinkFailure)] = &[
    ("31360", DlinkFailure::DlinkExpired),
    ("31326", DlinkFailure::HotlinkProtected),
];

/// 引擎错误文本 → dlink 类失败分类(卡内 ①);未命中 = None(调用方走通用
/// `classify_engine_failure` 路径,零猜测零吞错)。错误码为纯数字,包含匹配
/// 无大小写问题;不裁剪不变形,规则表单源。
#[must_use]
pub fn classify_dlink_failure(text: &str) -> Option<DlinkFailure> {
    DLINK_RULES
        .iter()
        .find(|(needle, _)| text.contains(needle))
        .map(|(_, failure)| *failure)
}

/// dlink 重取接口抽象(卡面边界:官方 rclone 无 baidu 后端、rc 面无 filemetas
/// 对应命令,oauth.rs:19-20 实机证据 → **真实现随 backends-go/WP11 落地补**;
/// Fake 实现仅测试,生产桩 = 违规)。
///
/// # 设计事实引用(docs/providers/baidu.md)
/// - 重取 = filemetas 族(`POST /xpan/file?method=filemetas` 路径数组 /
///   `GET /xpan/multimedia?method=filemetas` fsids ≤100,取 dlink 用这个,
///   baidu.md:39),无官方 rc 等价命令,故为 trait 接缝(T01 `RcDispatch` 先例:
///   生产实现由引擎/应用层适配);
/// - 所得 dlink 经下载链路消费:`GET <dlink>&access_token=...` 且必带
///   `User-Agent: pan.baidu.com`(缺 UA = 31326 防盗链,baidu.md:40);
/// - dlink 有时效,过期 = 31360(baidu.md:40/:68)→ 命中即重取换新。
pub trait DlinkRefetch {
    /// 重取 `target` 的可用 dlink。`target` = 文件标识文本(路径/fsid;真实形态
    /// WP11 实机锚定,Fake 注入任意串)。失败 severity 保真上浮,行零改动。
    fn refetch_dlink(&self, target: &str) -> Result<String, PartisyError>;
}

/// 重取编排结论(卡内 ② 三态,零吞错:每态机器可判定、携带可观测载荷)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DlinkRetryOutcome {
    /// 命中且余量未尽:重取→重建→回队(retries+1)→重新提交引擎成功;
    /// `record` = 重取后的新锚记录,`failure` = 结构化分类供 UI/日志。
    RefetchedAndRevived {
        /// revive_parked 成功后的最新记录(queued 带新锚,失败标注已清)。
        record: JobRecord,
        /// 本次命中的 dlink 失败分类。
        failure: DlinkFailure,
    },
    /// 命中但重试余量耗尽(retries ≥ max_retries):行保持 error 终态零改动,
    /// 手动处置/新任务属编排层决策;`failure` 供 UI 呈现失败性质。
    RetriesExhausted {
        /// 现行 error 终态记录。
        record: JobRecord,
        /// 本次命中的 dlink 失败分类。
        failure: DlinkFailure,
    },
    /// 错误文本非 dlink 类(分类未命中):编排器让位通用路径,行零改动。
    NotDlinkFailure {
        /// 现行记录(通常 error 终态,交通用处置)。
        record: JobRecord,
    },
}

/// dlink 失效重取编排骨架(卡内 ②;**骨架,非可用功能——真机验证挂 WP11**)。
/// 仅对 error 终态行操作(前置 = 编排层已经 [`JobManager::poll`] 观测失败落
/// 终态);行已被通用 Retryable 链回队(queued 停放)时不接手,见模块级
/// 「已知边界」。
///
/// # Errors
/// 行不存在/非 error 终态 = 结构化 Fatal;重取/重建失败 severity 保真上浮
/// (行零改动);回队/复活失败经守卫消歧后上浮。
pub fn dlink_refetch_retry(
    manager: &JobManager,
    dispatch: &impl RcDispatch,
    refetch: &impl DlinkRefetch,
    id: &str,
    target: &str,
    method: &str,
    rebuild: impl FnOnce(&str) -> Result<Value, PartisyError>,
) -> Result<DlinkRetryOutcome, PartisyError> {
    let record = manager.get(id)?;
    if !matches!(record.status, JobStatus::Error) {
        return Err(JobErrorKind::Invalid(format!(
            "dlink refetch retry requires an errored job: `{id}` is `{}`",
            record.status.as_str()
        ))
        .fatal());
    }
    let error_text = record.error.as_deref().unwrap_or("");
    let Some(failure) = classify_dlink_failure(error_text) else {
        return Ok(DlinkRetryOutcome::NotDlinkFailure { record });
    };
    // 上限沿 T01:行内 retries 对照 manager 配置策略(单一事实源,编排器零旁置)。
    if record.retries >= manager.retry_policy().max_retries {
        return Ok(DlinkRetryOutcome::RetriesExhausted { record, failure });
    }
    // 重取 → 重建(参数形状属调用方;core 校验 revive 可接受形状,避免回队后失败)。
    let fresh = refetch.refetch_dlink(target)?;
    let params = rebuild(&fresh)?;
    if !params.is_object() && !params.is_null() {
        return Err(JobErrorKind::Invalid(format!(
            "{method}: rebuilt dlink params must be a JSON object, got {params}"
        ))
        .fatal());
    }
    let parked = manager.requeue_for_dlink_refetch(id)?;
    let revived = manager.revive_parked(dispatch, &parked.id, method, &params)?;
    Ok(DlinkRetryOutcome::RefetchedAndRevived {
        record: revived,
        failure,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::*;
    use crate::error::Severity as Sev;
    use crate::jobs::{RetryPolicy, kind_of};

    /// 临时库管理器(dlink 模块不可直构 JobManager 字段;open_at 跑全迁移,
    /// 策略按用例可配;进程内自增后缀防多用例串库)。
    fn manager_with(policy: RetryPolicy) -> (JobManager, std::path::PathBuf) {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "partiverse-dlink-unit-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let manager = JobManager::open_at(&path)
            .expect("open job store")
            .with_retry_policy(policy);
        (manager, path)
    }

    /// Fake rc 分发(仅测试,Fake* 纪律):脚本化依序应答并记录调用。
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
            self.replies
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| JobErrorKind::Invalid("script exhausted".into()).fatal())
        }
    }

    /// Fake dlink 重取源(仅测试):固定返回新 dlink 并记录 target 调用。
    struct FakeDlinkRefetch {
        fresh: &'static str,
        targets: RefCell<Vec<String>>,
    }

    impl FakeDlinkRefetch {
        fn call_count(&self) -> usize {
            self.targets.borrow().len()
        }
    }

    impl DlinkRefetch for FakeDlinkRefetch {
        fn refetch_dlink(&self, target: &str) -> Result<String, PartisyError> {
            self.targets.borrow_mut().push(target.to_owned());
            Ok(self.fresh.to_owned())
        }
    }

    /// job/status 失败应答(finished+success=false+error 文本;文本不得含 T01
    /// 实机签名词,保证走「dlink 码 → 通用保守 fatal」路径)。
    fn failed(error: &str) -> Value {
        json!({ "finished": true, "success": false, "error": error, "output": {} })
    }

    /// job/status 运行中应答。
    fn running() -> Value {
        json!({ "finished": false, "success": false, "error": "" })
    }

    /// 两段 poll 至失败落终态(先 running 后失败文本;每次 poll 消费一条脚本应答,
    /// 带锚 queued 行首轮应 running,故必须两次 poll)。
    fn poll_to_failure(
        manager: &JobManager,
        id: &str,
        error: &str,
    ) -> Result<JobRecord, PartisyError> {
        let _ = manager.poll(&FakeScriptDispatch::scripted(&[running()]), id)?;
        Ok(manager
            .poll(&FakeScriptDispatch::scripted(&[failed(error)]), id)?
            .0)
    }

    /// 提交一枚 job(脚本化引擎应答;params 携带旧 dlink 供断言)。
    fn submit_job(
        manager: &JobManager,
        jobid: i64,
        old_dlink: &str,
    ) -> Result<JobRecord, PartisyError> {
        let dispatch = FakeScriptDispatch::scripted(&[json!({ "jobid": jobid })]);
        manager.submit(
            &dispatch,
            "sync/copy",
            "copy",
            old_dlink,
            "pvdst:",
            &json!({ "srcFs": old_dlink }),
        )
    }

    #[test]
    fn dlink_classification_is_table_driven_and_conservative() {
        // 卡内 ①:表驱动——规则表逐条命中(裸串+真实感包裹文本);decline 面 =
        // 通用错误/取消/111 异步进行中(baidu.md 事实,不入分类)/空文本;
        // 粗粒度已知例(数字串撞车)作为保守形态登记,WP11 收紧时须先改本断言。
        for (needle, expected) in DLINK_RULES {
            assert_eq!(classify_dlink_failure(needle), Some(*expected));
            let wrapped = format!("download failed: {{\"error_code\":{needle},\"errmsg\":\"x\"}}");
            assert_eq!(
                classify_dlink_failure(&wrapped),
                Some(*expected),
                "{needle}"
            );
        }
        assert_eq!(
            classify_dlink_failure(
                "download failed: {\"error_code\":31360,\"errmsg\":\"dlink has expired\"}"
            ),
            Some(DlinkFailure::DlinkExpired)
        );
        assert_eq!(
            classify_dlink_failure(
                "transfer aborted: {\"error_code\":31326,\"errmsg\":\"hotlink protection\"}"
            ),
            Some(DlinkFailure::HotlinkProtected)
        );
        for declined in [
            "directory not found",
            "user_canceled",
            "async task in progress: 111",
            "didn't find key \"srcFs\" in input",
            "",
        ] {
            assert_eq!(
                classify_dlink_failure(declined),
                None,
                "declined: {declined}"
            );
        }
        // 已知保守形态(登记):裸串包含匹配会撞无关数字串;收紧挂 WP11 实机文本。
        assert_eq!(
            classify_dlink_failure("size 1313600 mismatch"),
            Some(DlinkFailure::DlinkExpired),
            "登记例:粗粒度匹配的已知代价,收紧前不得静默改语义"
        );
        // 结构化面:码/canonical 串/severity(卡内裁定两类均 Retryable)。
        assert_eq!(DlinkFailure::DlinkExpired.code(), "31360");
        assert_eq!(DlinkFailure::HotlinkProtected.code(), "31326");
        assert_eq!(DlinkFailure::DlinkExpired.as_str(), "dlink_expired");
        assert_eq!(DlinkFailure::HotlinkProtected.as_str(), "hotlink_protected");
        assert_eq!(DlinkFailure::DlinkExpired.severity(), Sev::Retryable);
        assert_eq!(DlinkFailure::HotlinkProtected.severity(), Sev::Retryable);
    }

    #[test]
    fn dlink_refetch_retry_full_fake_chain_until_cap() {
        // 卡内 ②④:Fake 全链(31360)——分类命中 → 重取(target 保真)→ 重建
        // 参数注入新 dlink → 回队(retries+1)→ revive(新锚、标注清空);逐轮
        // 累计至上限,终局 RetriesExhausted 零触达、行零改动。
        let (manager, path) = manager_with(RetryPolicy {
            max_retries: 2,
            backoff: Duration::from_secs(60),
        });
        let error_text = "transfer failed: {\"error_code\":31360,\"errmsg\":\"dlink has expired\"}";
        let refetch = FakeDlinkRefetch {
            fresh: "fresh-dlink",
            targets: RefCell::new(Vec::new()),
        };
        let target = "baidu:/apps/partiverse/a.bin";
        // 首次提交(jobid=1)→ running → 31360 失败:通用分类保守落 error/fatal
        //(dlink 码不在 T01 实机签名表),重取余量未动。
        let job = submit_job(&manager, 1, "baidu-dl:old-dlink").expect("submit");
        let _ = manager
            .poll(&FakeScriptDispatch::scripted(&[running()]), &job.id)
            .expect("poll running");
        let (record, _) = manager
            .poll(
                &FakeScriptDispatch::scripted(&[failed(error_text)]),
                &job.id,
            )
            .expect("poll failed");
        assert_eq!(
            (record.status, record.severity, record.retries),
            (JobStatus::Error, Some(Sev::Fatal), 0)
        );
        // 第 1 轮重取编排:回队+复活,新 dlink 进参数。
        let revive1 = FakeScriptDispatch::scripted(&[json!({ "jobid": 2 })]);
        let outcome = dlink_refetch_retry(
            &manager,
            &revive1,
            &refetch,
            &job.id,
            target,
            "sync/copy",
            |fresh| Ok(json!({ "srcFs": format!("baidu-dl:{fresh}") })),
        )
        .expect("rescue 1");
        assert_eq!(
            outcome,
            DlinkRetryOutcome::RefetchedAndRevived {
                failure: DlinkFailure::DlinkExpired,
                record: manager.get(&job.id).expect("reread"),
            },
            "载荷 = 行内最新记录"
        );
        assert_eq!(
            refetch.targets.borrow().as_slice(),
            [target],
            "重取 target 保真"
        );
        let record = manager.get(&job.id).expect("record 1");
        assert_eq!(
            (record.status, record.engine_job_id, record.retries),
            (JobStatus::Queued, Some(2), 1),
            "回队 retries+1,复活带新锚"
        );
        assert_eq!(record.error.as_deref(), None, "复活清失败标注");
        assert_eq!(record.severity, None);
        let (method, params) = revive1.calls.borrow().last().expect("revive call").clone();
        assert_eq!(method, "sync/copy");
        assert_eq!(
            params,
            json!({ "srcFs": "baidu-dl:fresh-dlink", "_async": true }),
            "重建参数注入新 dlink"
        );
        // 第 2 轮:再失败 → 编排(余量 1)→ retries=2。
        let _ = poll_to_failure(&manager, &job.id, error_text).expect("poll round 2");
        let revive2 = FakeScriptDispatch::scripted(&[json!({ "jobid": 3 })]);
        let outcome = dlink_refetch_retry(
            &manager,
            &revive2,
            &refetch,
            &job.id,
            target,
            "sync/copy",
            |fresh| Ok(json!({ "srcFs": format!("baidu-dl:{fresh}") })),
        )
        .expect("rescue 2");
        assert!(matches!(
            outcome,
            DlinkRetryOutcome::RefetchedAndRevived { .. }
        ));
        assert_eq!(manager.get(&job.id).expect("record 2").retries, 2);
        // 第 3 轮:再失败 → 余量耗尽 → RetriesExhausted,重取/复活零触达,行不动。
        let _ = poll_to_failure(&manager, &job.id, error_text).expect("poll round 3");
        let before = manager.get(&job.id).expect("record 3");
        let probe = FakeScriptDispatch::scripted(&[]);
        let outcome = dlink_refetch_retry(
            &manager,
            &probe,
            &refetch,
            &job.id,
            target,
            "sync/copy",
            |fresh| Ok(json!({ "srcFs": format!("baidu-dl:{fresh}") })),
        )
        .expect("rescue 3");
        assert_eq!(
            outcome,
            DlinkRetryOutcome::RetriesExhausted {
                record: before.clone(),
                failure: DlinkFailure::DlinkExpired,
            }
        );
        assert_eq!(refetch.call_count(), 2, "余量耗尽不重取");
        assert_eq!(probe.calls.borrow().len(), 0, "余量耗尽不复活");
        assert_eq!(manager.get(&job.id).expect("record 3b"), before, "行零改动");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hotlink_variant_classifies_and_exhausts_independently() {
        // 31326 变体走同一骨架:分类结构化为 HotlinkProtected;上限=0 时首轮即
        // RetriesExhausted(重取零触达,行零改动)。
        let (manager, path) = manager_with(RetryPolicy {
            max_retries: 0,
            backoff: Duration::from_secs(60),
        });
        let refetch = FakeDlinkRefetch {
            fresh: "fresh-dlink",
            targets: RefCell::new(Vec::new()),
        };
        let job = submit_job(&manager, 11, "baidu-dl:old").expect("submit");
        let _ = poll_to_failure(
            &manager,
            &job.id,
            "transfer aborted: {\"error_code\":31326,\"errmsg\":\"hotlink\"}",
        )
        .expect("poll 31326 failure");
        let before = manager.get(&job.id).expect("record");
        let outcome = dlink_refetch_retry(
            &manager,
            &FakeScriptDispatch::scripted(&[]),
            &refetch,
            &job.id,
            "baidu:/apps/partiverse/a.bin",
            "sync/copy",
            |_| Ok(json!({})),
        )
        .expect("rescue attempt at cap 0");
        assert_eq!(
            outcome,
            DlinkRetryOutcome::RetriesExhausted {
                record: before.clone(),
                failure: DlinkFailure::HotlinkProtected,
            }
        );
        assert_eq!(refetch.call_count(), 0, "余量 0:重取零触达");
        assert_eq!(manager.get(&job.id).expect("reread"), before, "行零改动");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dlink_refetch_retry_rejects_non_error_rows() {
        // 卡内 ② 守卫面:queued/done 行拒绝(Fatal),重取零触达。
        let (manager, path) = manager_with(RetryPolicy::default());
        let refetch = FakeDlinkRefetch {
            fresh: "fresh-dlink",
            targets: RefCell::new(Vec::new()),
        };
        let parked = manager.register_queued("copy", "a:", "b:").expect("parked");
        let err = dlink_refetch_retry(
            &manager,
            &FakeScriptDispatch::scripted(&[]),
            &refetch,
            &parked.id,
            "t",
            "sync/copy",
            |_| Ok(json!({})),
        )
        .expect_err("queued rejected");
        assert_eq!(err.severity, Sev::Fatal);
        assert!(matches!(kind_of(&err), Some(JobErrorKind::Invalid(_))));
        let dispatch = FakeScriptDispatch::scripted(&[json!({ "jobid": 7 })]);
        let job = manager
            .submit(
                &dispatch,
                "sync/copy",
                "copy",
                "a:",
                "b:",
                &json!({ "srcFs": "a:" }),
            )
            .expect("submit");
        // poll 至 done(真实状态机路径;advance 为 jobs 模块私有,不经旁门)。
        let _ = manager
            .poll(
                &FakeScriptDispatch::scripted(&[json!({
                    "finished": true, "success": true, "error": "", "output": {}
                })]),
                &job.id,
            )
            .expect("poll to done");
        let err = dlink_refetch_retry(
            &manager,
            &FakeScriptDispatch::scripted(&[]),
            &refetch,
            &job.id,
            "t",
            "sync/copy",
            |_| Ok(json!({})),
        )
        .expect_err("done rejected");
        assert_eq!(err.severity, Sev::Fatal);
        assert_eq!(refetch.call_count(), 0, "拒绝面零重取");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dlink_refetch_retry_declines_unmatched_and_guards_rebuild() {
        // 补充守卫面:未命中(NotDlinkFailure)让位零触达;重建载荷非对象在回队
        // 前拦截(行零改动);重取失败 severity 保真上浮(行保持 error 可再救援)。
        let (manager, path) = manager_with(RetryPolicy::default());
        let refetch = FakeDlinkRefetch {
            fresh: "fresh-dlink",
            targets: RefCell::new(Vec::new()),
        };
        // job1:通用失败(未命中)→ error 终态 → NotDlinkFailure。
        let job1 = submit_job(&manager, 21, "baidu-dl:old").expect("submit 1");
        let _ = poll_to_failure(&manager, &job1.id, "directory not found").expect("poll to error");
        let before = manager.get(&job1.id).expect("record");
        let probe = FakeScriptDispatch::scripted(&[]);
        let outcome = dlink_refetch_retry(
            &manager,
            &probe,
            &refetch,
            &job1.id,
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
        assert_eq!(refetch.call_count(), 0, "未命中不重取");
        assert_eq!(probe.calls.borrow().len(), 0, "未命中不复活");
        assert_eq!(manager.get(&job1.id).expect("reread"), before, "行零改动");
        // job2:31360 命中 + 重建载荷非对象 → 回队前拦截(Fatal),行零改动。
        let job2 = submit_job(&manager, 22, "baidu-dl:old").expect("submit 2");
        let _ = poll_to_failure(
            &manager,
            &job2.id,
            "transfer failed: {\"error_code\":31360}",
        )
        .expect("poll dlink fail");
        let after_fail = manager.get(&job2.id).expect("record after fail");
        let probe = FakeScriptDispatch::scripted(&[]);
        let err = dlink_refetch_retry(
            &manager,
            &probe,
            &refetch,
            &job2.id,
            "t",
            "sync/copy",
            |_| Ok(json!(42)),
        )
        .expect_err("bad rebuild rejected");
        assert_eq!(err.severity, Sev::Fatal);
        assert_eq!(probe.calls.borrow().len(), 0, "拦截发生在回队前");
        assert_eq!(
            manager.get(&job2.id).expect("reread 2"),
            after_fail,
            "重建失败行零改动"
        );
        // 重取失败:severity 保真上浮(Retryable),行保持 error 终态可再救援。
        struct FakeFakeFailingRefetch;
        impl DlinkRefetch for FakeFailingRefetch {
            fn refetch_dlink(&self, _target: &str) -> Result<String, PartisyError> {
                Err(PartisyError::new(Sev::Retryable))
            }
        }
        let err = dlink_refetch_retry(
            &manager,
            &FakeScriptDispatch::scripted(&[]),
            &FakeFailingRefetch,
            &job2.id,
            "t",
            "sync/copy",
            |_| Ok(json!({})),
        )
        .expect_err("refetch failure floats");
        assert_eq!(err.severity, Sev::Retryable);
        assert_eq!(
            manager.get(&job2.id).expect("reread 3"),
            after_fail,
            "重取失败行零改动"
        );
        let _ = std::fs::remove_file(&path);
    }
}
