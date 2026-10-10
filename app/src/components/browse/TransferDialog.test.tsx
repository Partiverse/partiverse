// 预览-提交对话框组件行为单测(M1-WP06-T02 DoD⑤⑥ 无人值守轮,08 §4.4):
// 目标探测→同名冲突徽标;冲突四策略;重命名类在无服务端能力协商时显式禁用+
// 说明(禁静默降级);提交按规划组装 job_submit 元数据(单文件=父目录+FilterRule,
// 目录=整树)。Fake* 纪律:零真实 IPC。待 Owner GUI 实操验收,非静默标绿。
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { TransferDialog } from "./TransferDialog";
import type { SubmitMeta, TransferEntry } from "@/lib/transfer/model";

const fakes = vi.hoisted(() => ({
  calls: [] as Array<[string, unknown[]]>,
}));

vi.mock("@/bindings", () => ({
  commands: {
    operationsList: async (_fs: string) => {
      fakes.calls.push(["operationsList", [_fs]]);
      return {
        status: "ok",
        data: JSON.stringify({
          list: [{ Name: "dup.txt", IsDir: false, Size: 1, ModTime: "2026-10-10T00:00:00Z" }],
        }),
      };
    },
  },
}));

afterEach(cleanup);

const ENTRIES: TransferEntry[] = [
  { id: "/dup.txt", name: "dup.txt", isDir: false, sizeBytes: 2 },
  { id: "/fresh.txt", name: "fresh.txt", isDir: false, sizeBytes: 3 },
  { id: "/sub", name: "sub", isDir: true, sizeBytes: null },
];

function renderDialog(onSubmit: (metas: SubmitMeta[]) => Promise<void>): void {
  render(
    <TransferDialog
      open
      isMove={false}
      entries={ENTRIES}
      srcRoot="/"
      node="Local"
      onClose={() => undefined}
      onSubmit={onSubmit}
    />,
  );
}

describe("TransferDialog(预览-提交 + 冲突策略)", () => {
  it("目标探测后标记同名冲突;覆盖策略按规划提交(单文件 filter/目录整树)", async () => {
    fakes.calls = [];
    const submitted: SubmitMeta[][] = [];
    renderDialog(async (metas) => {
      submitted.push(metas);
    });
    const dst = screen.getByLabelText("Destination (remote:path or local path)");
    fireEvent.change(dst, { target: { value: "/backup" } });
    expect(await screen.findByText("exists at destination")).toBeTruthy();
    expect(screen.getAllByText("exists at destination").length).toBe(1);
    // 覆盖策略(默认):三项全提交。
    fireEvent.click(screen.getByRole("button", { name: "Submit 3" }));
    await waitFor(() => expect(submitted.length).toBe(1));
    const metas = submitted[0];
    expect(metas.length).toBe(3);
    const file = metas.find((meta) => meta.src === "/dup.txt");
    expect(JSON.parse(file?.params ?? "{}")).toEqual({
      srcFs: "/", dstFs: "/backup", _filter: { FilterRule: ["+ /dup.txt", "- *"] },
    });
    const dir = metas.find((meta) => meta.src === "/sub");
    expect(dir?.dst).toBe("/backup/sub");
    expect(JSON.parse(dir?.params ?? "{}")).toEqual({ srcFs: "/sub", dstFs: "/backup/sub" });
    expect(metas.every((meta) => meta.node === "Local" && meta.cost === 1 && meta.method === "sync/copy")).toBe(true);
  });

  it("skip 策略剔除冲突项(提交计数变化);重命名类无能力时禁用+说明,禁静默", async () => {
    fakes.calls = [];
    renderDialog(async () => undefined);
    const dst = screen.getByLabelText("Destination (remote:path or local path)");
    fireEvent.change(dst, { target: { value: "/backup" } });
    await screen.findByText("exists at destination");
    // skip:dup.txt 被剔除 → 仅 2 项提交。
    fireEvent.click(screen.getByLabelText("Skip"));
    expect(screen.getByRole("button", { name: "Submit 2" })).toBeTruthy();
    // rename/keep-both:radio 禁用 + 说明文案(WP09 能力协商前不静默改名)。
    for (const label of ["Rename", "Keep both"]) {
      const radio = screen.getByLabelText(label) as HTMLInputElement;
      expect(radio.disabled).toBe(true);
    }
    expect(screen.getAllByText(/negotiated server-side copy capability/).length).toBeGreaterThan(0);
  });

  it("目标探测失败显式上浮且禁止提交(禁静默直通)", async () => {
    fakes.calls = [];
    // 用例独立桩:替换 operationsList 应答为错误信封,结束后还原。
    const mod = await import("@/bindings");
    const original = mod.commands.operationsList;
    Object.assign(mod.commands, {
      operationsList: async () => ({
        status: "error" as const,
        error: { kind: "engine", msg: "source fs unresolvable", severity: "fatal" },
      }),
    });
    try {
      renderDialog(async () => undefined);
      fireEvent.change(screen.getByLabelText("Destination (remote:path or local path)"), {
        target: { value: "/nowhere" },
      });
      expect(await screen.findByText(/Destination check failed/)).toBeTruthy();
      expect((screen.getByRole("button", { name: "Submit 3" }) as HTMLButtonElement).disabled).toBe(true);
    } finally {
      Object.assign(mod.commands, { operationsList: original });
    }
  });
});
