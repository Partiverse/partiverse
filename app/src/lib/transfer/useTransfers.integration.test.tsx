// 传输 store 跨消费者集成测试(对抗审查修复轮,钉死 [high] split-brain 回归):
// 提交链(模拟 BrowseView 经 useTransfers().submit 提交)与真实 TransferBar 必须
// 共享同一 store 单例——①提交产生的失败行在传输条内重试启用并按原元数据调
// job_retry(旧双实例实现:提交方元数据写入私有实例,传输条 hasMeta 恒 false →
// 重试恒禁用 + 「another session」tooltip 事实错误);②Throttled 挂起行对传输条
// 可见(旧:仅提交方私有,传输条永不显示「Rate limited…」);③提交方卸载(路由
// 切换语义)后元数据随单例存活;④多消费者恒单 2s 轮询环(旧:双环双倍
// jobPoll+core/stats 触达)。组件层组合 + Fake* 纪律:零真实 IPC。
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";

import { TransferBar } from "@/components/shell/TransferBar";
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
  /** jobList 触达计数(单轮询环断言用)。 */
  jobListCount: 0,
  /** jobSubmit 应答:"submitted"(默认)或 { wait_until_ms }(Throttled 信封)。 */
  submitReply: { mode: "submitted", waitUntilMs: 0 },
}));

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
      fakes.jobListCount += 1;
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
      // 提交即入列且引擎侧失败(severity=fatal):验证「失败行内重试」全链。
      const record = job(`j${fakes.jobs.length + 1}`, {
        status: "error", error: "boom: is a file not a directory", severity: "fatal",
      });
      fakes.jobs.push(record);
      return { status: "ok", data: { submitted: record } };
    },
    jobRetry: async (...args: unknown[]) => {
      fakes.calls.push(["jobRetry", args]);
      return { status: "ok", data: { submitted: job("j-retried", { status: "running", engine_job_id: 11 }) } };
    },
    jobCancel: async (id: string) => {
      fakes.calls.push(["jobCancel", [id]]);
      return { status: "ok", data: job(id, { status: "error", error: "user_canceled", severity: "interrupted" }) };
    },
  },
}));

const META: SubmitMeta = {
  node: "Local", method: "sync/copy", kind: "copy", src: "/a.txt", dst: "/backup",
  params: '{"srcFs":"/","dstFs":"/backup","_filter":{"FilterRule":["+ /a.txt","- *"]}}', cost: 1,
};

/** 提交链替身(BrowseView 同款消费面:useTransfers().submit → refresh)。 */
function SubmitPane(): React.JSX.Element {
  const transfers = useTransfers();
  const [verdict, setVerdict] = useState<string | null>(null);
  const onSubmit = async (): Promise<void> => {
    const result = await transfers.submit(META);
    setVerdict(result);
    // 与真实用户节奏一致:提交后立即可拉一轮(不等 2s 轮询),双消费者同帧可见。
    await transfers.refresh();
  };
  return (
    <section aria-label="submit-fixture">
      <button type="button" onClick={() => { void onSubmit(); }}>submit-fixture</button>
      <output>{verdict}</output>
    </section>
  );
}

/** 第二订阅者替身(多消费者单环断言用)。 */
function PollProbe(): null {
  useTransfers();
  return null;
}

beforeEach(() => {
  fakes.jobs = [];
  fakes.calls = [];
  fakes.jobListCount = 0;
  fakes.submitReply = { mode: "submitted", waitUntilMs: 0 };
  resetTransfersForTests();
});

afterEach(() => {
  cleanup();
  resetTransfersForTests();
});

describe("传输 store 跨消费者集成(提交→条内重试)", () => {
  it("提交链与传输条共享元数据:失败行内重试启用并按原元数据调 job_retry", async () => {
    render(
      <>
        <SubmitPane />
        <TransferBar />
      </>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Transfers" }));
    expect(await screen.findByText("Queue is empty")).toBeTruthy();
    // 提交(BrowseView 同款路径)→ 行进入传输条队列面板(旧双实例:永不出现)。
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "submit-fixture" }));
    });
    expect(await screen.findByText("submitted")).toBeTruthy();
    expect(await screen.findByText("Failed")).toBeTruthy();
    expect(screen.getByText("a: → b:")).toBeTruthy();
    // 关键断言:重试启用(旧双实例实现此处 disabled=true)+ tooltip 无「另一会话」。
    const retry = screen.getByRole("button", { name: "Retry" });
    expect((retry as HTMLButtonElement).disabled).toBe(false);
    expect((retry as HTMLButtonElement).title ?? "").not.toContain("another session");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    });
    const retryCall = fakes.calls.find(([name]) => name === "jobRetry");
    expect(retryCall).toBeDefined();
    expect(retryCall?.[1]).toEqual([
      "j1", META.node, META.method, META.kind, META.src, META.dst, META.params, META.cost,
    ]);
  });

  it("Throttled 挂起行对传输条可见(限流挂起,非提交方私有)", async () => {
    // 未到期挂起(wait_until_ms 在未来):入列后停留,不触发自动重提交。
    fakes.submitReply = { mode: "throttled", waitUntilMs: Date.now() + 60_000 };
    render(
      <>
        <SubmitPane />
        <TransferBar />
      </>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Transfers" }));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "submit-fixture" }));
    });
    expect(await screen.findByText("throttled")).toBeTruthy();
    // 旧双实例实现:挂起行只在提交方实例,传输条面板永不渲染该行。
    expect(await screen.findByText(/Rate limited/)).toBeTruthy();
    const note = screen.getByRole("note");
    expect(note.textContent).toContain("rate-limiting");
  });

  it("提交方卸载(路由切换语义)后元数据随单例存活:传输条重试仍启用", async () => {
    const view = render(
      <>
        <SubmitPane key="submitter" />
        <TransferBar key="bar" />
      </>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Transfers" }));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "submit-fixture" }));
    });
    expect(await screen.findByText("Failed")).toBeTruthy();
    // App.tsx 平级路由切换:提交方(browse 视图)卸载,传输条常驻保留。
    view.rerender(
      <>
        <TransferBar key="bar" />
      </>,
    );
    // 旧双实例实现:元数据随提交方实例销毁,此处 disabled=true。
    const retry = screen.getByRole("button", { name: "Retry" });
    expect((retry as HTMLButtonElement).disabled).toBe(false);
    await act(async () => {
      fireEvent.click(retry);
    });
    const retryCall = fakes.calls.find(([name]) => name === "jobRetry");
    expect(retryCall?.[1]).toEqual([
      "j1", META.node, META.method, META.kind, META.src, META.dst, META.params, META.cost,
    ]);
  });

  it("多消费者恒单轮询环:双消费者下 jobList 每 2s 触达一次(旧双实例=双倍)", async () => {
    vi.useFakeTimers();
    try {
      render(
        <>
          <PollProbe />
          <TransferBar />
        </>,
      );
      // 订阅即挂环,但首轮刷新在宏任务:此刻零触达。
      expect(fakes.jobListCount).toBe(0);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(1);
      });
      // 挂载首轮:单环=1 次(旧双实例实现:双首轮=2 次)。
      expect(fakes.jobListCount).toBe(1);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(2_000);
      });
      // 一个 2s 周期:仍单环(累计 2;旧实现累计 4,双倍 jobPoll+core/stats 触达)。
      expect(fakes.jobListCount).toBe(2);
    } finally {
      vi.useRealTimers();
    }
  });
});
