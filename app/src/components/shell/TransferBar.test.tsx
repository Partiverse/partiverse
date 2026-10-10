// 传输条组件行为单测(M1-WP06-T02 DoD⑥ 无人值守轮,08 §4.4 对照):聚合进度、
// 队列面板可解释状态(限速中/排队-预算器/重试中/失败)、失败行查看原因与重试
// 按钮(跨会话行显式禁用,禁静默)。Fake* 纪律:零真实 IPC。
// 【非 GUI 验收记录】待 Owner GUI 实操验收(AGENTS 硬规则),非静默标绿。
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { TransferBar } from "./TransferBar";
import type { JobRecordOut } from "@/bindings";

const fakes = vi.hoisted(() => ({
  jobs: [] as Array<Record<string, unknown>>,
  calls: [] as Array<[string, unknown[]]>,
}));

function job(id: string, patch: Partial<JobRecordOut>): JobRecordOut {
  return {
    id, kind: "copy", src: "a:", dst: "b:", status: "queued", engine_job_id: null,
    error: null, severity: null, created_at: "2026-10-10T00:00:00.000Z",
    updated_at: "2026-10-10T00:00:00.000Z", progress_bytes: null, progress_total: null,
    checksum: null, retries: 0, ...patch,
  };
}

vi.mock("@/bindings", () => ({
  commands: {
    jobList: async () => ({ status: "ok", data: fakes.jobs }),
    jobPoll: async (id: string) => {
      fakes.calls.push(["jobPoll", [id]]);
      const record = fakes.jobs.find((candidate) => candidate.id === id);
      if (record === undefined) throw new Error(`unknown job ${id}`);
      return { status: "ok", data: { record, output: null } };
    },
    jobSubmit: async () => {
      throw new Error("jobSubmit is not expected in bar tests");
    },
    jobRetry: async (...args: unknown[]) => {
      fakes.calls.push(["jobRetry", args]);
      return { status: "error", error: { kind: "core", msg: "no meta", severity: "fatal" } };
    },
    jobCancel: async (id: string) => {
      fakes.calls.push(["jobCancel", [id]]);
      return { status: "ok", data: job(id, { status: "error", error: "user_canceled", severity: "interrupted" }) };
    },
  },
}));

afterEach(cleanup);

describe("TransferBar(08 §4.4)", () => {
  it("空队列显示 idle,展开面板显示空态", async () => {
    fakes.jobs = [];
    fakes.calls = [];
    render(<TransferBar />);
    expect(await screen.findByText("No active transfers")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Transfers" }));
    expect(await screen.findByText("Queue is empty")).toBeTruthy();
  });

  it("聚合进度=有总量活跃行求和;队列四类可解释状态齐备", async () => {
    fakes.jobs = [
      job("j1", { status: "running", engine_job_id: 1, src: "a:", dst: "b:", progress_bytes: 50, progress_total: 200 }),
      job("j2", { status: "queued", src: "c:", dst: "d:" }),
      job("j3", { status: "queued", retries: 2, error: "couldn't list files: 429", severity: "retryable", src: "e:", dst: "f:" }),
      job("j4", { status: "error", error: "is a file not a directory", severity: "fatal", src: "g:", dst: "h:" }),
      job("j5", { status: "done", src: "i:", dst: "j:" }),
    ];
    render(<TransferBar />);
    // 活跃 = queued+running(j1 running 带总量;聚合 50/200 = 25%)。
    expect(await screen.findByText("3 active · 25%")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Transfers" }));
    expect(await screen.findByText("Transferring")).toBeTruthy();
    expect(screen.getByText("Queued — budget scheduler")).toBeTruthy();
    expect(screen.getByText("Retrying")).toBeTruthy();
    expect(screen.getByText("Failed")).toBeTruthy();
    expect(screen.getByText("Completed")).toBeTruthy();
    // 方向/源→目标渲染(move 用 ⇄)。
    expect(screen.getByText("g: → h:")).toBeTruthy();
    // 进度列百分比。
    expect(screen.getByText("25%")).toBeTruthy();
    // 失败行:查看原因展开 + 重试按钮(跨会话无元数据 → 显式禁用)。
    const retry = screen.getByRole("button", { name: "Retry" });
    expect((retry as HTMLButtonElement).disabled).toBe(true);
    expect((retry as HTMLButtonElement).title).toContain("another session");
    fireEvent.click(screen.getByRole("button", { name: "Details" }));
    const note = await screen.findByRole("note");
    expect(note.textContent).toContain("is a file not a directory");
  });
});
