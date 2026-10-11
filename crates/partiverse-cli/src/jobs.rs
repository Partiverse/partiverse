//! jobs 子命令(M1-WP07-T02 卡内 ①):list [--status] [--watch] / poll / cancel /
//! reconcile / retry,全部透传 core `JobManager`,CLI 零业务逻辑;错误 severity
//! 保真上浮(各函数不再重复 Errors 节)。--watch 轮询间隔固定 1s(卡内边界),
//! 终止条件 = 无 running 且无带锚 queued(停放 queued 无锚行如实入镜但不阻塞
//! 退出,零死循环)。retry 元数据红线 3:行内不落 method/params,CLI 保守默认 =
//! method 按 kind 映射(copy→sync/copy,move→sync/move)、params =
//! `{"srcFs": src, "dstFs": dst}`(本 CLI copy/move 的原始提交形状),均可
//! `--method`/`--params` 显式覆盖。

use std::time::Duration;

use partiverse_core::budget::{BudgetScheduler, JobSpec};
use partiverse_core::error::PartisyError;
use partiverse_core::fsops::poll_with_progress;
use partiverse_core::jobs::{JobManager, JobRecord, JobRetryOutcome, JobStatus, RcDispatch};
use serde_json::{Value, json};

use crate::fsops::{budget_node, fatal, throttled_text};

/// 观察未安定行:running 或带锚 queued(停放 queued 无锚不阻塞 --watch 退出)。
fn is_busy(record: &JobRecord) -> bool {
    match record.status {
        JobStatus::Running => true,
        JobStatus::Queued => record.engine_job_id.is_some(),
        JobStatus::Done | JobStatus::Error => false,
    }
}

/// 单行记录面(字段均为路径与元数据:零文件内容,零凭据面)。
fn format_record(record: &JobRecord) -> String {
    let progress = match (record.progress_bytes, record.progress_total) {
        (Some(bytes), Some(total)) => format!(" {bytes}/{total}B"),
        (Some(bytes), None) => format!(" {bytes}B"),
        (None, _) => String::new(),
    };
    let anchor = record
        .engine_job_id
        .map_or("-".to_owned(), |id| id.to_string());
    format!(
        "{}\t{}\t{}\t{} -> {}{}\tengine:{}\t{}",
        record.id,
        record.kind,
        record.status.as_str(),
        record.src,
        record.dst,
        progress,
        anchor,
        record.error.as_deref().unwrap_or(""),
    )
}

/// `jobs list [--status <s>] [--watch]`:`JobManager::list_jobs`;--watch 每 1s
/// 重照,快照变化才入转写(库面零直写 stdout)。
pub fn list(
    manager: &JobManager,
    status: Option<JobStatus>,
    watch: bool,
) -> Result<String, PartisyError> {
    let mut transcript: Vec<String> = Vec::new();
    let mut last = String::new();
    loop {
        let records = manager.list_jobs(status)?;
        let snapshot = records
            .iter()
            .map(format_record)
            .collect::<Vec<_>>()
            .join("\n");
        if snapshot != last {
            transcript.push(snapshot.clone());
            last = snapshot;
        }
        if !watch || !records.iter().any(is_busy) {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(transcript.join("\n---\n"))
}

/// `jobs poll <id>`:core `poll_with_progress`(状态机推进 + 采样落进度列)。
pub fn poll(
    manager: &JobManager,
    dispatch: &impl RcDispatch,
    id: &str,
) -> Result<String, PartisyError> {
    let (record, _) = poll_with_progress(manager, dispatch, id)?;
    Ok(format_record(&record))
}

/// `jobs cancel <id>`:core `job_cancel`(终态裁决 = error/user_canceled/Interrupted)。
pub fn cancel(
    manager: &JobManager,
    dispatch: &impl RcDispatch,
    id: &str,
) -> Result<String, PartisyError> {
    let record = manager.job_cancel(dispatch, id)?;
    Ok(format!("canceled: {}", format_record(&record)))
}

/// `jobs reconcile`:core `reconcile`(孤儿 running 行 → error/engine_restarted)。
pub fn reconcile(manager: &JobManager, dispatch: &impl RcDispatch) -> Result<String, PartisyError> {
    let reconciled = manager.reconcile(dispatch)?;
    if reconciled.is_empty() {
        return Ok("reconcile: no orphan running rows".to_owned());
    }
    Ok(reconciled
        .iter()
        .map(|record| format!("reconciled: {}", format_record(record)))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `jobs retry <id> [--method <m>] [--params <json>] [--node <n>] [--cost <n>]`:
/// core `JobManager::retry`(壳 commands.rs 决策树纯提取单源);kind/src/dst 取
/// 行内,node 默认 = 源端 [`budget_node`];`--params` 非 JSON / kind 无默认
/// method → 结构化 Fatal。
// 壳 commands.rs job_retry 同款先例:透传参数面(语义各别,强打包反损可读)。
#[allow(clippy::too_many_arguments)]
pub fn retry(
    manager: &JobManager,
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    id: &str,
    node: Option<&str>,
    cost: u64,
    method: Option<&str>,
    params_text: Option<&str>,
) -> Result<String, PartisyError> {
    let record = manager.get(id)?;
    let method = match method {
        Some(method) => method.to_owned(),
        None => match record.kind.as_str() {
            "copy" => "sync/copy".to_owned(),
            "move" => "sync/move".to_owned(),
            other => {
                return Err(fatal(format!(
                    "job `{id}` kind `{other}` has no default method; pass --method"
                )));
            }
        },
    };
    let params: Value = match params_text {
        Some(text) => serde_json::from_str(text)
            .map_err(|err| fatal(format!("--params must be valid JSON text: {err}")))?,
        None => json!({ "srcFs": record.src, "dstFs": record.dst }),
    };
    let node = node.unwrap_or_else(|| budget_node(&record.src));
    let outcome = manager.retry(
        dispatch,
        budget,
        id,
        node,
        cost,
        JobSpec {
            method: &method,
            kind: &record.kind,
            src: &record.src,
            dst: &record.dst,
            params: &params,
        },
    )?;
    Ok(match outcome {
        JobRetryOutcome::Revived(r) => format!("revived: {}", format_record(&r)),
        JobRetryOutcome::Submitted(r) => {
            format!("new job submitted (old row kept): {}", format_record(&r))
        }
        JobRetryOutcome::Throttled { wait_until_ms } => throttled_text(wait_until_ms),
        JobRetryOutcome::Exhausted(r) => {
            format!(
                "budget exhausted; job parked: {} (queued, engine untouched)",
                r.id
            )
        }
    })
}
