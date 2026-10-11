//! 文件操作子命令(M1-WP07-T02 卡内 ①):mkdir / rm / copy / move,全部透传
//! core `fsops`,CLI 零业务逻辑;预算三态原样上浮、错误 severity 保真(各函数
//! 不再重复 Errors 节)。rm = 预览-提交两段(破坏性操作禁直通):[`rm_preview`]
//! 列目标,stdin 确认收口在 main.rs(只认 `yes`),确认后 [`rm_apply`] 才触
//! 引擎。`--wait` 轮询至终态(1s 间隔,core 零 sleep)。

use std::time::Duration;

use partiverse_core::budget::{BudgetScheduler, GatedSubmit};
use partiverse_core::error::{PartisyError, Severity};
use partiverse_core::fsops::poll_with_progress;
use partiverse_core::fsops::{self, FsOpOutcome, SingleFileOutcome, SingleFileSpec, TransferSpec};
use partiverse_core::jobs::{JobManager, JobStatus, RcDispatch};
use serde_json::json;

use crate::browse::{render_listing, split_target};

/// 结构化 Fatal 构造(connection.rs 同款先例:std io 错误为 source 根因)。
pub(crate) fn fatal(message: impl Into<String>) -> PartisyError {
    PartisyError::with_source(
        Severity::Fatal,
        Box::new(std::io::Error::other(message.into())),
    )
}

/// 预算计量 Node 键:具名 remote = 冒号前名;裸本地路径 = 路径本身(无 quota
/// profile = 不限流,`BudgetScheduler::acquire` 既有语义)。
pub(crate) fn budget_node(fs: &str) -> &str {
    match fs.split_once(':') {
        Some((name, _)) => name,
        None => fs,
    }
}

/// 预算 Throttled 可见文案(卡内 ③ 三态 CLI 可见;时刻 = unix 毫秒)。
pub(crate) fn throttled_text(wait_until_ms: u64) -> String {
    format!("budget throttled: retry after unix ms {wait_until_ms} (nothing sent to engine)")
}

/// 预算 Exhausted 可见文案(结构性拒绝:cost 超桶容)。
fn exhausted_text() -> String {
    "budget exhausted: cost exceeds bucket capacity (nothing sent to engine)".to_owned()
}

/// `mkdir <remote:path>`:core `fs_mkdir`(具名 remote fs = 基座;裸路径 fs = 路径本身)。
pub fn mkdir(
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    target: &str,
    cost: u64,
) -> Result<String, PartisyError> {
    let (fs, remote) = split_target(target);
    match fsops::fs_mkdir(budget, dispatch, budget_node(&fs), &fs, &remote, cost)? {
        FsOpOutcome::Applied => Ok(format!("directory created: {target}")),
        FsOpOutcome::Throttled { wait_until_ms } => Ok(throttled_text(wait_until_ms)),
        FsOpOutcome::Exhausted => Ok(exhausted_text()),
    }
}

/// `rm` 预览段:operations/list 列目标(fs+remote 两键齐传,T09 实锚形状)+
/// 删除语义告知;stdin 确认在 main.rs 收口。target listing 经 T03 渲染器出
/// 人类可读表格(与 `ls` 同函数复用,卡内 ②)。
pub fn rm_preview(dispatch: &impl RcDispatch, target: &str) -> Result<String, PartisyError> {
    let (fs, remote) = split_target(target);
    let reply = dispatch.call("operations/list", &json!({ "fs": fs, "remote": remote }))?;
    let listing = render_listing(&reply).join("\n");
    Ok(format!(
        "about to delete ALL FILES under `{target}` (engine semantics: files removed, \
         directory shell kept; a file target fails on the engine)\n--- target listing ---\n{listing}"
    ))
}

/// `rm` 提交段(仅确认后调用):core `fs_delete`(fs 携带全路径,实测接受)。
pub fn rm_apply(
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    target: &str,
    cost: u64,
) -> Result<String, PartisyError> {
    match fsops::fs_delete(budget, dispatch, budget_node(target), target, cost)? {
        FsOpOutcome::Applied => Ok(format!(
            "deleted: {target} (files removed, directory shell kept)"
        )),
        FsOpOutcome::Throttled { wait_until_ms } => Ok(throttled_text(wait_until_ms)),
        FsOpOutcome::Exhausted => Ok(exhausted_text()),
    }
}

/// `copy|move <srcFs> <dstFs>`:默认树传输 = core `transfer_submit`(gated_submit
/// 全链);`--file <src_remote>` 单文件 = core `single_file_transfer`(无 caps
/// 保守路径 P9,重命名不可表达 = 同名落盘);`--wait` 轮询至终态(1s 间隔)。
/// `--file` 无文件名 → 结构化 Fatal。
// 壳 commands.rs job_retry 同款先例:透传参数面(语义各别,强打包反损可读)。
#[allow(clippy::too_many_arguments)]
pub fn transfer(
    manager: &JobManager,
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    is_move: bool,
    src: &str,
    dst: &str,
    file: Option<&str>,
    cost: u64,
    wait: bool,
) -> Result<String, PartisyError> {
    let kind = if is_move { "move" } else { "copy" };
    let outcome = match file {
        None => fsops::transfer_submit(
            manager,
            budget,
            dispatch,
            &TransferSpec {
                node: budget_node(src),
                cost,
                is_move,
                kind,
                src_fs: src,
                dst_fs: dst,
            },
        )?,
        Some(src_remote) => {
            let name = src_remote
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("");
            if name.is_empty() {
                return Err(fatal(format!(
                    "--file must name a file inside `{src}`, got {src_remote:?}"
                )));
            }
            match fsops::single_file_transfer(
                manager,
                budget,
                dispatch,
                &SingleFileSpec {
                    node: budget_node(src),
                    cost,
                    is_move,
                    kind,
                    src_fs_base: src,
                    src_remote,
                    dst_fs: dst,
                    // 重命名在 SyncJob 保守路径不可表达(fsops 卡内裁定):同名落盘。
                    dst_remote: name,
                    src_caps: None,
                    dst_caps: None,
                },
            )? {
                SingleFileOutcome::Submitted(record) => GatedSubmit::Submitted(record),
                SingleFileOutcome::Throttled { wait_until_ms } => {
                    GatedSubmit::Throttled { wait_until_ms }
                }
                SingleFileOutcome::Exhausted(record) => GatedSubmit::Exhausted(record),
                SingleFileOutcome::SyncApplied => {
                    return Ok(format!("{kind} applied (server-side single file)"));
                }
            }
        }
    };
    match outcome {
        GatedSubmit::Submitted(record) => wait_or_report(manager, dispatch, &record.id, wait),
        GatedSubmit::Throttled { wait_until_ms } => Ok(throttled_text(wait_until_ms)),
        GatedSubmit::Exhausted(record) => Ok(format!(
            "{}; job parked: {} (queued, engine untouched; revive with: jobs retry {})",
            exhausted_text(),
            record.id,
            record.id
        )),
    }
}

/// 提交后报告(无 `--wait` = 单次 poll 回执;`--wait` = 1s 间隔轮询至终态,
/// 经 core `poll_with_progress` 顺带采样进度列)。输出 = 全程转写文本(库面
/// 零直写 stdout,main.rs 统一打印);poll/采样失败 severity 保真上浮。
fn wait_or_report(
    manager: &JobManager,
    dispatch: &impl RcDispatch,
    id: &str,
    wait: bool,
) -> Result<String, PartisyError> {
    let mut lines = vec![format!("job submitted: {id}")];
    let mut last = Option::<JobStatus>::None;
    loop {
        let (record, _) = poll_with_progress(manager, dispatch, id)?;
        if last != Some(record.status) {
            lines.push(format!("{id}: {}", record.status.as_str()));
            last = Some(record.status);
        }
        match record.status {
            JobStatus::Done => {
                lines.push(format!("job done: {id}"));
                return Ok(lines.join("\n"));
            }
            JobStatus::Error => {
                lines.push(format!(
                    "job error: {id} ({})",
                    record.error.as_deref().unwrap_or("<no error text>")
                ));
                return Ok(lines.join("\n"));
            }
            // 无 --wait:单次 poll 回执(状态如实,零等待)。
            _ if wait => std::thread::sleep(Duration::from_secs(1)),
            _ => return Ok(lines.join("\n")),
        }
    }
}

/// rm 预览渲染复用单测(T03 卡内 ②同函数面;引擎形状由真引擎集成测试锚定)。
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Fake* 测试替身(仅测试命名惯例):固定应答一枚目录+文件 listing。
    struct FakeListing;

    impl RcDispatch for FakeListing {
        fn call(&self, method: &str, _params: &Value) -> Result<Value, PartisyError> {
            assert_eq!(method, "operations/list");
            Ok(json!({
                "list": [
                    { "Name": "sub", "IsDir": true, "Size": 0,
                      "ModTime": "2026-10-11T09:00:00+08:00" },
                    { "Name": "a.txt", "IsDir": false, "Size": 2048,
                      "ModTime": "2026-10-11T09:30:00+08:00" }
                ]
            }))
        }
    }

    /// rm 预览的 target listing = `ls` 同款表格(非裸 JSON),目录恒前排。
    #[test]
    fn rm_preview_renders_target_listing_table() {
        let out = rm_preview(&FakeListing, "pvlocal:doomed").expect("preview ok");
        assert!(out.contains("--- target listing ---"), "{out}");
        assert!(
            out.lines().any(|line| line.starts_with("NAME")),
            "表头在列: {out}"
        );
        assert!(
            out.contains("sub") && out.contains("a.txt") && out.contains("2.0 KiB"),
            "条目行渲染(字节人类可读): {out}"
        );
        assert!(
            out.find("sub").expect("dir row present")
                < out.find("a.txt").expect("file row present"),
            "目录恒前排: {out}"
        );
        assert!(
            !out.contains("{\"list\""),
            "预览 listing 不得为裸 JSON: {out}"
        );
    }
}
