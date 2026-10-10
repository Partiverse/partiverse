// 传输纯逻辑层(M1-WP06-T02 DoD④⑤,零 React/IPC 依赖,与 browse/model.ts 同
// 层惯例):可解释状态推导(08 §4.4:限速中-该盘限流/排队-预算器/重试中)、
// 聚合进度、预览-提交冲突规划(跳过/覆盖/重命名/两者保留)、rc 参数组装。
// rc 形状与 crates/partiverse-core/src/fsops.rs 的 1.75.1 实机锚定一致
// (srcFs 指向文件必败 → 单文件 = 源父目录 + _filter.FilterRule)。
import type { JobRecordOut } from "@/bindings";

/** 行内提交元数据(job_submit/job_retry 供参;行内不落 params——红线 3,
 * 由本会话传输 store 保存,跨会话行重试不可用并显式禁用)。 */
export interface SubmitMeta {
  node: string;
  method: string;
  kind: string;
  src: string;
  dst: string;
  params: string;
  cost: number;
}

/** 预算门 Throttled 的挂起行(零落库;客户端计时到点后自动重提交)。 */
export interface PendingThrottle {
  waitUntilMs: number;
  meta: SubmitMeta;
}

/** 队列面板行:挂起行(预算限流)或持久 job 行。 */
export interface QueueRow {
  key: string;
  record: JobRecordOut | null;
  pending: PendingThrottle | null;
}

/** 可解释状态:i18n 键 + 附因(失败原因原文/重试说明数据源)。 */
export interface Explain {
  key: string;
  detail: string | null;
}

/** 状态→可解释文案推导(§4.4/§5 说人话,禁黑箱术语):
 * error=user_canceled → 已取消;error → 失败+原因;done → 完成;
 * running → 传输中;queued 带锚 → 提交窗瞬态;queued 无锚+retries>0 →
 * 重试中(附上次失败原因);queued 无锚 → 排队-预算器(停放语义)。 */
export function explainStatus(record: JobRecordOut): Explain {
  if (record.status === "error") {
    if (record.error === "user_canceled") {
      return { key: "transfers.state.canceled", detail: null };
    }
    return { key: "transfers.state.failed", detail: record.error ?? null };
  }
  if (record.status === "done") return { key: "transfers.state.completed", detail: null };
  if (record.status === "running") return { key: "transfers.state.running", detail: null };
  if (record.engine_job_id !== null) return { key: "transfers.state.queued", detail: null };
  if (record.retries > 0) {
    return { key: "transfers.state.retrying", detail: record.error ?? null };
  }
  return { key: "transfers.state.queuedBudget", detail: null };
}

/** 底部条聚合进度:只聚合 queued/running 行;有总量的行求和,全无总量 →
 * indeterminate(totalBytes=null,禁造值)。 */
export interface Aggregate {
  activeCount: number;
  bytes: number;
  totalBytes: number | null;
}

export function aggregateProgress(rows: readonly QueueRow[]): Aggregate {
  let activeCount = 0;
  let bytes = 0;
  let total = 0;
  let hasTotal = false;
  for (const row of rows) {
    const record = row.record;
    if (record === null || (record.status !== "running" && record.status !== "queued")) continue;
    activeCount += 1;
    if (record.progress_total !== null && record.progress_total > 0) {
      hasTotal = true;
      total += record.progress_total;
      bytes += record.progress_bytes ?? 0;
    }
  }
  return { activeCount, bytes, totalBytes: hasTotal ? total : null };
}

/** 进度百分数(整数 0-100;无总量 → null,调用方显示不确定态)。 */
export function progressPercent(record: JobRecordOut): number | null {
  if (record.progress_total === null || record.progress_total <= 0) return null;
  return Math.min(100, Math.floor(((record.progress_bytes ?? 0) * 100) / record.progress_total));
}

// ── 冲突规划(§4.4 预览-提交:跳过/覆盖/重命名/两者保留)──

export type ConflictPolicy = "skip" | "overwrite" | "rename" | "keep-both";

export interface TransferEntry {
  id: string;
  name: string;
  isDir: boolean;
  sizeBytes: number | null;
}

/** 自动改名 "name (1).ext" 起步,跳过 taken 集合(两者保留/重命名共用)。 */
export function autoRename(name: string, taken: ReadonlySet<string>): string {
  const dot = name.lastIndexOf(".");
  const base = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : "";
  for (let i = 1; ; i += 1) {
    const candidate = `${base} (${i})${ext}`;
    if (!taken.has(candidate)) return candidate;
  }
}

export interface PlanItem {
  entry: TransferEntry;
  action: "submit" | "skip";
  /** 目标名(仅服务端单文件路径可改;同路径同名落盘恒等于 entry.name)。 */
  dstName: string;
  /** 该项的落盘语义需要服务端单文件能力(改名)或与策略无关。 */
  needsServerSideRename: boolean;
}

export interface Plan {
  items: PlanItem[];
  /** 无 caps 协商时被拒的改名项数(UI 显式禁用重命名类选项,禁静默降级)。 */
  renamesBlocked: number;
}

/** 冲突策略规划(纯函数):冲突 = 目标目录已存在的同名文件(目录按 rclone
 * 合并语义,不算冲突)。skip → 冲突项剔除;overwrite → 原样提交;rename/
 * keep-both → 自动改名目标——但改名落盘只能在「服务端单文件」路径表达
 * (core fsops 实测:sync/copy 过滤器路径同名落盘,改名不可表达);无 caps
 * 协商(serverSideAvailable=false)时计入 renamesBlocked 并跳过。 */
export function planTransfer(
  entries: readonly TransferEntry[],
  dstNames: ReadonlySet<string>,
  policy: ConflictPolicy,
  serverSideAvailable: boolean,
): Plan {
  const items: PlanItem[] = [];
  let renamesBlocked = 0;
  const taken = new Set(dstNames);
  for (const entry of entries) {
    const conflict = dstNames.has(entry.name);
    if (!conflict || policy === "overwrite" || entry.isDir) {
      taken.add(entry.name);
      items.push({ entry, action: "submit", dstName: entry.name, needsServerSideRename: false });
      continue;
    }
    if (policy === "skip") {
      items.push({ entry, action: "skip", dstName: entry.name, needsServerSideRename: false });
      continue;
    }
    if (!serverSideAvailable) {
      renamesBlocked += 1;
      items.push({ entry, action: "skip", dstName: entry.name, needsServerSideRename: true });
      continue;
    }
    const renamed = autoRename(entry.name, taken);
    taken.add(renamed);
    items.push({ entry, action: "submit", dstName: renamed, needsServerSideRename: true });
  }
  return { items, renamesBlocked };
}

// ── rc 参数组装(job_submit 供参;形状 = core fsops 实测锚定)──

/** fs 串拼接(与 core fsops::compose_fs 同规则:base 以 : / / 结尾直接续)。 */
export function composeFs(base: string, path: string): string {
  if (path === "") return base;
  if (base.endsWith(":") || base.endsWith("/")) return `${base}${path}`;
  return `${base}/${path}`;
}

/** remote 相对路径父目录("a/b.txt" → "a";根级文件 → "";空 → null)。 */
export function parentRemote(remote: string): string | null {
  const trimmed = remote.replace(/\/+$/, "");
  if (trimmed === "") return null;
  const cut = trimmed.lastIndexOf("/");
  return cut === -1 ? "" : trimmed.slice(0, cut);
}

/** 目录/整树传输参数(sync/copy|move:目录合并语义)。 */
export function dirTransferParams(srcFs: string, dstFs: string): string {
  return JSON.stringify({ srcFs, dstFs });
}

/** 单文件传输参数(1.75.1 实测:srcFs=源父目录 + FilterRule 精确选中,
 * srcFs 指向文件本身必败 "is a file not a directory")。 */
export function singleFileParams(srcParentFs: string, dstFs: string, name: string): string {
  return JSON.stringify({
    srcFs: srcParentFs,
    dstFs,
    _filter: { FilterRule: [`+ /${name}`, "- *"] },
  });
}
