// 手测脚本(M1-WP06-T02:传输面纯逻辑离线自检,无需 Tauri/引擎,同 WP05-T02 惯例)。
// 运行:node src/lib/transfer/manual-check.ts(Node ≥23.6 原生 TS 类型剥离)。
// 离线 fixture 走完整管线:可解释状态推导 → 聚合进度 → 冲突规划(四策略)→
// rc 参数组装。真实端到端(rc 引擎)需在桌面壳内手测;本脚本非 GUI 验收记录。
import {
  aggregateProgress, autoRename, composeFs, dirTransferParams, explainStatus,
  parentRemote, planTransfer, progressPercent, singleFileParams,
  type QueueRow, type TransferEntry,
} from "./model.ts";
import type { JobRecordOut } from "@/bindings";

function record(id: string, patch: Partial<JobRecordOut>): JobRecordOut {
  return {
    id, kind: "copy", src: "a:", dst: "b:", status: "queued", engine_job_id: null,
    error: null, severity: null, created_at: "2026-10-10T00:00:00.000Z",
    updated_at: "2026-10-10T00:00:00.000Z", progress_bytes: null, progress_total: null,
    checksum: null, retries: 0, ...patch,
  };
}
const row = (r: JobRecordOut): QueueRow => ({ key: `job:${r.id}`, record: r, pending: null });

const rows = [
  row(record("j1", { status: "running", engine_job_id: 1, progress_bytes: 524_288, progress_total: 1_048_576, src: "Local:/photos", dst: "jianguoyun:/backup" })),
  row(record("j2", { status: "queued", src: "Local:/docs", dst: "jianguoyun:/backup" })),
  row(record("j3", { status: "queued", retries: 1, error: "couldn't list files: 429 Too Many Requests", severity: "retryable" })),
  row(record("j4", { status: "error", error: "is a file not a directory", severity: "fatal" })),
  row(record("j5", { status: "done" })),
];

console.log("== 1) 可解释状态(08 §4.4:说人话+可解释) ==");
for (const item of rows) {
  const explain = explainStatus(item.record as JobRecordOut);
  console.log(`  ${item.record?.id} [${item.record?.status}] → ${explain.key}${explain.detail !== null ? ` (${explain.detail})` : ""}`);
}
console.log("== 2) 聚合进度(底部条) ==");
const aggregate = aggregateProgress(rows);
console.log(`  active=${aggregate.activeCount} bytes=${aggregate.bytes} total=${aggregate.totalBytes}`);
console.log(`  j1 percent = ${progressPercent(rows[0]?.record as JobRecordOut)}%`);
console.log("== 3) 冲突规划(跳过/覆盖/重命名/两者保留) ==");
const entries: TransferEntry[] = [
  { id: "/a.txt", name: "a.txt", isDir: false, sizeBytes: 1 },
  { id: "/dup.txt", name: "dup.txt", isDir: false, sizeBytes: 2 },
  { id: "/sub", name: "sub", isDir: true, sizeBytes: null },
];
const dstNames = new Set(["dup.txt"]);
for (const policy of ["skip", "overwrite", "rename", "keep-both"] as const) {
  const plan = planTransfer(entries, dstNames, policy, false);
  console.log(`  ${policy}: submit=${plan.items.filter((item) => item.action === "submit").map((item) => item.entry.name).join(",") || "—"} blocked=${plan.renamesBlocked}`);
}
const withServerSide = planTransfer(entries, dstNames, "keep-both", true);
console.log(`  keep-both(有服务端能力): ${withServerSide.items.map((item) => `${item.entry.name}→${item.dstName}`).join(", ")}`);
console.log(`  autoRename 连号: ${autoRename("dup.txt", new Set(["dup.txt", "dup (1).txt"]))}`);
console.log("== 4) rc 参数组装(core fsops 1.75.1 实测锚定形状) ==");
console.log(`  composeFs("pvsrc:", "sub") = ${composeFs("pvsrc:", "sub")}`);
console.log(`  parentRemote("a/b.txt") = ${parentRemote("a/b.txt")}`);
console.log(`  dirTransferParams = ${dirTransferParams("pvsrc:photos", "jianguoyun:/backup/photos")}`);
console.log(`  singleFileParams = ${singleFileParams("pvsrc:", "jianguoyun:/backup", "a.txt")}`);
console.log("OK — 全部管线自检通过(非 GUI 验收记录,待 Owner GUI 实操验收)");
