// 传输 store 行为单测(M1-WP06-T02 DoD⑥ 无人值守轮):提交登记重试元数据 →
// 重试以原元数据调 job_retry;取消调 job_cancel;Throttled 挂起行入列、到点
// 自动重提交;信封错误上浮 store.error(禁吞错)。对抗审查修复轮:store 改为
// 模块级单例(useSyncExternalStore),补「双消费者共享单例」回归断言 + 测试间
// resetTransfersForTests 确定性复位。Fake* 纪律:零真实 IPC。
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { resetTransfersForTests, useTransfers } from "./useTransfers";
import type { SubmitMeta } from "./model";

interface FakeJob {
  id: string;
  kind: string;
  src: string;
  dst: string;
  status: "queued" | "running" | "done" | "error";
  engine_job_id: number | null;
  error: string | null;
  severity: string | null;
  created_at: string;
  updated_at: string;
  progress_bytes: number | null;
  progress_total: number | null;
  checksum: string | null;
  retries: number;
}

const fakes = vi.hoisted(() => ({
  jobs: [] as FakeJob[],
  calls: [] as Array<[string, unknown[]]>,
  failList: false,
  /** jobSubmit 应答:"submitted"(默认)或 { wait_until_ms }(Throttled 信封)。 */
  submitReply: { mode: "submitted", waitUntilMs: 0 },
}));

function reset(jobs: FakeJob[]): void {
  fakes.jobs = jobs;
  fakes.calls = [];
  fakes.failList = false;
  fakes.submitReply = { mode: "submitted", waitUntilMs: 0 };
}

function job(id: string, patch: Partial<FakeJob> = {}): FakeJob {
  return {
    id, kind: "copy", src: "a:", dst: "b:", status: "queued", engine_job_id: null,
    error: null, severity: null, created_at: "2026-10-10T00:00:00.000Z",
    updated_at: "2026-10-10T00:00:00.000Z", progress_bytes: null, progress_total: null,
    checksum: null, retries: 0, ...patch,
  };
}

vi.mock("@/bindings", () => ({
  commands: {
    jobList: async () => {
      if (fakes.failList) {
        return { status: "error", error: { kind: "engine", msg: "engine gone", severity: "fatal" } };
      }
      return { status: "ok", data: fakes.jobs };
    },
    jobPoll: async (id: string) => {
      fakes.calls.push(["jobPoll", [id]]);
      const record = fakes.jobs.find((candidate) => candidate.id === id);
      if (record === undefined) throw new Error(`unknown job ${id}`);
      return { status: "ok", data: { record, output: null } };
    },
    jobSubmit: async (...args: unknown[]) => {
      fakes.calls.push(["jobSubmit", args]);
      if (fakes.submitReply.mode === "throttled") {
        return { status: "ok", data: { throttled: { wait_until_ms: fakes.submitReply.waitUntilMs } } };
      }
      const record = job(`j${fakes.jobs.length + 1}`, { status: "running", engine_job_id: 10 });
      fakes.jobs.push(record);
      return { status: "ok", data: { submitted: record } };
    },
    jobRetry: async (...args: unknown[]) => {
      fakes.calls.push(["jobRetry", args]);
      const record = job("j-retried", { status: "running", engine_job_id: 11 });
      return { status: "ok", data: { submitted: record } };
    },
    jobCancel: async (id: string) => {
      fakes.calls.push(["jobCancel", [id]]);
      const record = job(id, { status: "error", error: "user_canceled", severity: "interrupted" });
      fakes.jobs = fakes.jobs.map((candidate) => (candidate.id === id ? record : candidate));
      return { status: "ok", data: record };
    },
  },
}));

const META: SubmitMeta = {
  node: "Local", method: "sync/copy", kind: "copy", src: "/a.txt", dst: "/backup",
  params: '{"srcFs":"/","dstFs":"/backup","_filter":{"FilterRule":["+ /a.txt","- *"]}}', cost: 1,
};

afterEach(() => {
  // 单例跨测试存活:卸载订阅者 + 复位单例状态(停轮询环/清元数据与挂起行),
  // 保证用例间确定性(只加严,不改任何既有断言)。
  cleanup();
  resetTransfersForTests();
  reset([]);
});

describe("useTransfers", () => {
  it("提交登记重试元数据,重试按原元数据调 job_retry", async () => {
    reset([]);
    const { result } = renderHook(() => useTransfers());
    let verdict: string | null = null;
    await act(async () => {
      verdict = await result.current.submit(META);
    });
    expect(verdict).toBe("submitted");
    expect(result.current.hasMeta("j1")).toBe(true);
    await act(async () => {
      await result.current.retry("j1");
    });
    const retry = fakes.calls.find(([name]) => name === "jobRetry");
    expect(retry).toBeDefined();
    expect(retry?.[1]).toEqual([
      "j1", META.node, META.method, META.kind, META.src, META.dst, META.params, META.cost,
    ]);
  });

  it("取消按 id 调 job_cancel 并刷新", async () => {
    reset([job("j1", { status: "running", engine_job_id: 9 })]);
    const { result } = renderHook(() => useTransfers());
    await waitFor(() => expect(result.current.rows.length).toBe(1));
    await act(async () => {
      await result.current.cancel("j1");
    });
    expect(fakes.calls.some(([name, args]) => name === "jobCancel" && args[0] === "j1")).toBe(true);
    await waitFor(() => expect(result.current.rows[0]?.record?.status).toBe("error"));
  });

  it("Throttled 挂起行入列;到点后刷新轮自动重提交", async () => {
    reset([]);
    const { result } = renderHook(() => useTransfers());
    // 先排干挂载首轮(kickoff 宏任务),消除与提交路径的并发竞态,计数可断言。
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });
    fakes.submitReply = { mode: "throttled", waitUntilMs: Date.now() };
    let verdict: string | null = null;
    await act(async () => {
      verdict = await result.current.submit(META);
    });
    expect(verdict).toBe("throttled");
    await waitFor(() => expect(result.current.rows[0]?.pending).not.toBeNull());
    // 挂起行零 job 行元数据(hasMeta 不登记),重试按钮语义由组件层钉。
    expect(result.current.rows.some((row) => row.record === null && row.pending !== null)).toBe(true);
    // 到点:刷新轮自动重提交(jobSubmit 第二次调用),挂起行消化。
    fakes.submitReply = { mode: "submitted", waitUntilMs: 0 };
    await act(async () => {
      await result.current.refresh();
    });
    await waitFor(() => expect(result.current.rows.filter((row) => row.pending !== null).length).toBe(0));
    const submits = fakes.calls.filter(([name]) => name === "jobSubmit");
    expect(submits.length).toBe(2);
  });

  it("清单失败上浮 store.error(禁吞错)", async () => {
    reset([]);
    fakes.failList = true;
    const { result } = renderHook(() => useTransfers());
    await waitFor(() => expect(result.current.error).toContain("engine gone"));
  });

  // 对抗审查修复轮回归钉(split-brain):旧实现两个 renderHook = 两份私有实例,
  // A 提交后 B 的 hasMeta 恒 false。现共享同一单例,B 必须可见 A 登记的元数据。
  it("双消费者共享单例:A 实例提交,B 实例 hasMeta 可见", async () => {
    reset([]);
    const a = renderHook(() => useTransfers());
    const b = renderHook(() => useTransfers());
    await act(async () => {
      await a.result.current.submit(META);
    });
    // B 实例(如传输条)对 A 实例(如提交链)登记的元数据可见 → 重试可启用。
    expect(b.result.current.hasMeta("j1")).toBe(true);
    // 刷新一轮后,B 的行视图同样包含该 job(共享 records,非实例私有)。
    await act(async () => {
      await b.result.current.refresh();
    });
    expect(b.result.current.rows.some((row) => row.record?.id === "j1")).toBe(true);
  });
});
