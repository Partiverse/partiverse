// 传输纯逻辑单测(M1-WP06-T02 DoD⑥ 无人值守轮;测试即规格):可解释状态、
// 聚合进度、冲突规划、参数组装(rc 形状与 core fsops 实测锚定一致)。
import { describe, expect, it } from "vitest";

import {
  aggregateProgress, autoRename, composeFs, dirTransferParams, explainStatus,
  parentRemote, planTransfer, progressPercent, singleFileParams,
  type QueueRow, type TransferEntry,
} from "./model";
import type { JobRecordOut } from "@/bindings";

function record(patch: Partial<JobRecordOut>): JobRecordOut {
  return {
    id: "j1", kind: "copy", src: "a:", dst: "b:", status: "queued",
    engine_job_id: null, error: null, severity: null,
    created_at: "2026-10-10T00:00:00.000Z", updated_at: "2026-10-10T00:00:00.000Z",
    progress_bytes: null, progress_total: null, checksum: null, retries: 0,
    ...patch,
  };
}

describe("可解释状态推导(§4.4/§5 说人话)", () => {
  it("八类状态各归其位,失败/重试附因", () => {
    expect(explainStatus(record({ status: "error", error: "user_canceled" })).key).toBe("transfers.state.canceled");
    const failed = explainStatus(record({ status: "error", error: "boom: 502" }));
    expect(failed.key).toBe("transfers.state.failed");
    expect(failed.detail).toBe("boom: 502");
    expect(explainStatus(record({ status: "done" })).key).toBe("transfers.state.completed");
    expect(explainStatus(record({ status: "running", engine_job_id: 7 })).key).toBe("transfers.state.running");
    // 带锚 queued = 提交窗瞬态;无锚 = 停放。
    expect(explainStatus(record({ status: "queued", engine_job_id: 7 })).key).toBe("transfers.state.queued");
    expect(explainStatus(record({ status: "queued" })).key).toBe("transfers.state.queuedBudget");
    const retrying = explainStatus(record({ status: "queued", retries: 1, error: "429" }));
    expect(retrying.key).toBe("transfers.state.retrying");
    expect(retrying.detail).toBe("429");
  });
});

describe("聚合进度与百分数", () => {
  const row = (record: JobRecordOut): QueueRow => ({ key: `job:${record.id}`, record, pending: null });
  it("只聚合活跃行,有总量求和,全无总量 = 不确定态", () => {
    const rows = [
      row(record({ id: "r", status: "running", progress_bytes: 100, progress_total: 200 })),
      row(record({ id: "q", status: "queued", progress_bytes: 10, progress_total: 50 })),
      row(record({ id: "d", status: "done", progress_bytes: 999, progress_total: 999 })),
      row(record({ id: "n", status: "running" })),
    ];
    const aggregate = aggregateProgress(rows);
    expect(aggregate.activeCount).toBe(3);
    expect(aggregate.bytes).toBe(110);
    expect(aggregate.totalBytes).toBe(250);
    expect(aggregateProgress([row(record({ id: "x", status: "running" }))]).totalBytes).toBeNull();
  });
  it("百分数:无总量/零总量 → null;上界 100", () => {
    expect(progressPercent(record({ progress_bytes: 1, progress_total: 0 }))).toBeNull();
    expect(progressPercent(record({ progress_bytes: 250, progress_total: 200 }))).toBe(100);
    expect(progressPercent(record({ progress_bytes: 50, progress_total: 200 }))).toBe(25);
  });
});

describe("冲突规划(跳过/覆盖/重命名/两者保留)", () => {
  const entries: TransferEntry[] = [
    { id: "/a.txt", name: "a.txt", isDir: false, sizeBytes: 1 },
    { id: "/dup.txt", name: "dup.txt", isDir: false, sizeBytes: 2 },
    { id: "/sub", name: "sub", isDir: true, sizeBytes: null },
  ];
  const dst = new Set(["dup.txt", "other.bin"]);

  it("skip 剔除冲突项,非冲突照常", () => {
    const plan = planTransfer(entries, dst, "skip", false);
    expect(plan.items.map((i) => [i.entry.name, i.action])).toEqual([
      ["a.txt", "submit"], ["dup.txt", "skip"], ["sub", "submit"],
    ]);
    expect(plan.renamesBlocked).toBe(0);
  });
  it("overwrite 冲突项原样提交;目录同名按合并语义不算冲突", () => {
    const plan = planTransfer(entries, dst, "overwrite", false);
    expect(plan.items.filter((i) => i.action === "submit").map((i) => i.entry.name)).toEqual(["a.txt", "dup.txt", "sub"]);
  });
  it("无服务端能力时 rename/keep-both 显式计数拒办(禁静默改名)", () => {
    for (const policy of ["rename", "keep-both"] as const) {
      const plan = planTransfer(entries, dst, policy, false);
      expect(plan.renamesBlocked).toBe(1);
      expect(plan.items.find((i) => i.entry.name === "dup.txt")?.action).toBe("skip");
    }
  });
  it("有服务端能力时自动改名并避让已占名(两者保留)", () => {
    const plan = planTransfer(entries, dst, "keep-both", true);
    const dup = plan.items.find((i) => i.entry.name === "dup.txt");
    expect(dup).toMatchObject({ action: "submit", dstName: "dup (1).txt", needsServerSideRename: true });
    expect(plan.renamesBlocked).toBe(0);
  });
  it("autoRename 连号避让", () => {
    const taken = new Set(["a.txt", "a (1).txt", "a (2).txt"]);
    expect(autoRename("a.txt", taken)).toBe("a (3).txt");
    expect(autoRename("noext", new Set(["noext"]))).toBe("noext (1)");
  });
});

describe("rc 参数组装(core fsops 实测锚定形状)", () => {
  it("composeFs 拼接与父目录解析", () => {
    expect(composeFs("pvsrc:", "sub")).toBe("pvsrc:sub");
    expect(composeFs("/tmp/x", "d")).toBe("/tmp/x/d");
    expect(parentRemote("a/b.txt")).toBe("a");
    expect(parentRemote("b.txt")).toBe("");
    expect(parentRemote("")).toBeNull();
  });
  it("目录参数 = {srcFs,dstFs}", () => {
    expect(dirTransferParams("a:", "b:")).toBe(JSON.stringify({ srcFs: "a:", dstFs: "b:" }));
  });
  it("单文件参数 = 源父目录 + FilterRule(srcFs 指向文件必败,实测锚定)", () => {
    expect(singleFileParams("pvsrc:sub", "pvdst:", "b.txt")).toBe(
      JSON.stringify({ srcFs: "pvsrc:sub", dstFs: "pvdst:", _filter: { FilterRule: ["+ /b.txt", "- *"] } }),
    );
  });
});
