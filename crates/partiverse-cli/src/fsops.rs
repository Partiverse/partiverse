//! 文件操作子命令(M1-WP07-T02 卡内 ①;M1-WP06-T05 判型分流改造):mkdir /
//! rm / copy / move,全部透传 core `fsops`,CLI 零业务逻辑;预算三态原样上浮、
//! 错误 severity 保真(各函数不再重复 Errors 节)。T05:copy/move 无 `--file`
//! 时走 core `typed_transfer` 自动判型(stat 探源:文件 → copyfile/movefile
//! 单文件通道;目录 → sync 编排 + 空源提示 D8;缺失 → 按卡边界目录路由);
//! rm 判型路由(文件 → operations/deletefile、目录 → purge 整树)+ 预览文案改
//! 「will delete the entire directory tree」(D6)。rm = 预览-提交两段(破坏性
//! 操作禁直通):[`rm_preview`] 判型列目标,stdin 确认收口在 main.rs(只认
//! `yes`),确认后 [`rm_apply`] 才触引擎。`--wait` 轮询至终态(1s 间隔,core
//! 零 sleep)。

use std::time::Duration;

use partiverse_core::budget::{BudgetScheduler, GatedSubmit};
use partiverse_core::error::{PartisyError, Severity};
use partiverse_core::fsops::poll_with_progress;
use partiverse_core::fsops::{
    self, EntryKind, FsOpOutcome, SingleFileOutcome, SingleFileSpec, TypedTransferOutcome,
    TypedTransferSpec, deletion_kind_fallback, stat_kind,
};
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

/// `rm` 预览段(T05 卡内 ② D6:stat 判型分语义):文件 → 单文件删除告知;
/// 目录 → 整树删除告知 + 目标 listing(与 `ls` 同函数复用);stat 缺失 → 尾段
/// 形态缺省(卡内边界),缺省形态不列表(引擎无对象可列,列操作必败;预览仍进
/// 确认门,apply 时引擎错误如实上浮)。stdin 确认在 main.rs 收口。
pub fn rm_preview(dispatch: &impl RcDispatch, target: &str) -> Result<String, PartisyError> {
    let (fs, remote) = split_target(target);
    let stat = stat_kind(dispatch, &fs, &remote)?;
    match stat.unwrap_or_else(|| deletion_kind_fallback(&remote)) {
        EntryKind::File => Ok(format!(
            "about to delete `{target}`: single file (engine operations/deletefile \
             removes the file itself)"
        )),
        EntryKind::Directory => {
            let listing = match stat {
                Some(_) => {
                    let reply =
                        dispatch.call("operations/list", &json!({ "fs": fs, "remote": remote }))?;
                    render_listing(&reply).join("\n")
                }
                // 缺省目录形态(stat 未确认存在):不列表,零引擎二次触达。
                None => "<stat reports the target does not exist; listing skipped>".to_owned(),
            };
            Ok(format!(
                "about to delete `{target}`: will delete the entire directory tree \
                 (engine operations/purge removes all files AND the directory itself)\n\
                 --- target listing ---\n{listing}"
            ))
        }
    }
}

/// `rm` 提交段(仅确认后调用):core `fs_remove` 判型路由——文件 → deletefile、
/// 目录 → purge 整树(实锚语义,见 core fsops 模块文档);apply 前重新判型
/// (预览与提交之间目标可能变化,以提交时点为准,零陈旧路由)。
pub fn rm_apply(
    budget: &BudgetScheduler,
    dispatch: &impl RcDispatch,
    target: &str,
    cost: u64,
) -> Result<String, PartisyError> {
    let (fs, remote) = split_target(target);
    let kind =
        stat_kind(dispatch, &fs, &remote)?.unwrap_or_else(|| deletion_kind_fallback(&remote));
    match fsops::fs_remove(budget, dispatch, budget_node(&fs), &fs, &remote, cost, kind)? {
        FsOpOutcome::Applied => Ok(match kind {
            EntryKind::File => format!("deleted: {target} (file removed)"),
            EntryKind::Directory => format!(
                "deleted: {target} (entire directory tree removed, including the \
                 directory itself)"
            ),
        }),
        FsOpOutcome::Throttled { wait_until_ms } => Ok(throttled_text(wait_until_ms)),
        FsOpOutcome::Exhausted => Ok(exhausted_text()),
    }
}

/// `copy|move <srcFs> <dstFs>`:无 `--file` = core `typed_transfer` 自动判型
/// (stat 探源:文件 → copyfile/movefile 单文件通道,dst 已存在目录 → 拷入保持
/// 名,否则精确路径;目录 → 目录级 sync 编排,空源输出提示 D8;缺失 → 按卡边界
/// 目录路由)。`--file <src_remote>` 显式单文件 = 既有 `single_file_transfer`
/// 协商路径(语义不变)。`--wait` 轮询至终态(1s 间隔)。`--file` 无文件名 →
/// 结构化 Fatal。
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
    // 空源提示行(D8):判型目录路由时由 core 旗标驱动,先于作业回执渲染。
    let mut prefix: Vec<String> = Vec::new();
    let gated = match file {
        None => {
            let (src_fs, src_remote) = split_target(src);
            let (dst_fs, dst_remote) = split_target(dst);
            match fsops::typed_transfer(
                manager,
                budget,
                dispatch,
                &TypedTransferSpec {
                    node: budget_node(&src_fs),
                    cost,
                    is_move,
                    kind,
                    src_fs: &src_fs,
                    src_remote: &src_remote,
                    dst_fs: &dst_fs,
                    dst_remote: &dst_remote,
                },
            )? {
                TypedTransferOutcome::FileApplied { dst_remote } => {
                    return Ok(format!(
                        "{kind} applied (single-file channel, engine dstRemote: {dst_remote})"
                    ));
                }
                TypedTransferOutcome::FileThrottled { wait_until_ms } => {
                    return Ok(throttled_text(wait_until_ms));
                }
                TypedTransferOutcome::FileParked(record) => {
                    return Ok(format!(
                        "{}; job parked: {} (queued, engine untouched; revive with: jobs retry {})",
                        exhausted_text(),
                        record.id,
                        record.id
                    ));
                }
                TypedTransferOutcome::Sync {
                    empty_source,
                    gated,
                } => {
                    if empty_source {
                        prefix.push("source directory is empty; nothing to transfer".to_owned());
                    }
                    gated
                }
            }
        }
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
    let report = match gated {
        GatedSubmit::Submitted(record) => wait_or_report(manager, dispatch, &record.id, wait)?,
        GatedSubmit::Throttled { wait_until_ms } => throttled_text(wait_until_ms),
        GatedSubmit::Exhausted(record) => format!(
            "{}; job parked: {} (queued, engine untouched; revive with: jobs retry {})",
            exhausted_text(),
            record.id,
            record.id
        ),
    };
    if prefix.is_empty() {
        return Ok(report);
    }
    prefix.push(report);
    Ok(prefix.join("\n"))
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

/// rm 预览渲染单测(T05 判型面):目录 → 整树文案+listing 表格;文件 → 单文件
/// 文案零 listing;stat 缺失 → 尾段形态缺省。rc 形状由真引擎集成测试锚定。
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use serde_json::Value;

    /// Fake* 测试替身(仅测试命名惯例):脚本化应答(stat 判型 + list)。
    struct FakeScripted {
        replies: std::cell::RefCell<VecDeque<Value>>,
        calls: std::cell::RefCell<Vec<(String, Value)>>,
    }

    impl FakeScripted {
        fn scripted(replies: &[Value]) -> Self {
            Self {
                replies: std::cell::RefCell::new(replies.iter().cloned().collect()),
                calls: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl RcDispatch for FakeScripted {
        fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError> {
            self.calls
                .borrow_mut()
                .push((method.to_owned(), params.clone()));
            self.replies
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| fatal("script exhausted"))
        }
    }

    #[test]
    fn rm_preview_detects_kind_and_renders_accordingly() {
        // 目录:stat → list → 整树文案(卡面钦定短句)+ listing 表格(非裸 JSON,
        // 目录恒前排)。
        let dir = FakeScripted::scripted(&[
            json!({ "item": { "Name": "doomed", "IsDir": true } }),
            json!({ "list": [
                { "Name": "sub", "IsDir": true, "Size": 0,
                  "ModTime": "2026-10-11T09:00:00+08:00" },
                { "Name": "a.txt", "IsDir": false, "Size": 2048,
                  "ModTime": "2026-10-11T09:30:00+08:00" }
            ]}),
        ]);
        let out = rm_preview(&dir, "pvlocal:doomed").expect("preview ok");
        assert!(
            out.contains("will delete the entire directory tree"),
            "卡面钦定文案在列: {out}"
        );
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
        // stat 双键形状。
        assert_eq!(
            dir.calls.borrow()[0].clone(),
            (
                "operations/stat".into(),
                json!({ "fs": "pvlocal:", "remote": "doomed" })
            )
        );
        // 文件:stat → 单文件文案,零 list 触达。
        let file = FakeScripted::scripted(&[json!({ "item": { "IsDir": false } })]);
        let out = rm_preview(&file, "pvlocal:doomed.txt").expect("preview ok");
        assert!(out.contains("single file"), "{out}");
        assert!(!out.contains("entire directory tree"), "{out}");
        assert_eq!(file.calls.borrow().len(), 1, "文件预览零 list");
        // stat 缺失 + 尾段含点 → 文件缺省形态,零 list。
        let ghost_file = FakeScripted::scripted(&[json!({ "item": null })]);
        let out = rm_preview(&ghost_file, "pvlocal:ghost.txt").expect("preview ok");
        assert!(out.contains("single file"), "{out}");
        assert_eq!(ghost_file.calls.borrow().len(), 1);
        // stat 缺失 + 尾段无点 → 目录缺省形态,不列表(零二次触达)。
        let ghost_dir = FakeScripted::scripted(&[json!({ "item": null })]);
        let out = rm_preview(&ghost_dir, "pvlocal:ghost").expect("preview ok");
        assert!(out.contains("entire directory tree"), "{out}");
        assert!(out.contains("listing skipped"), "{out}");
        assert_eq!(ghost_dir.calls.borrow().len(), 1);
    }

    /// rm 预览的 delete/purge 路由形状由 core 单测 + 真引擎集成测试锚定
    /// (cli 面零业务逻辑,不重复脚本)。
    #[test]
    fn delete_kind_fallback_matches_core_tail_form() {
        // 再导出面冒烟:CLI 引用的 core 缺省函数可用(单源)。
        assert_eq!(deletion_kind_fallback("a.txt"), EntryKind::File);
    }
}
