// GUI 归档脚本(DoD⑤)无人值守轮:jsdom 组件渲染快照落 docs/screenshots/。
// 【非 GUI 验收记录】产出物显式标注「待 Owner GUI 实操验收」,不静默标绿;
// GUI_ARCHIVE=1 时才写文件(常规测试轮零副作用),渲染断言始终执行。
// Owner 桌面轮:GUI_ARCHIVE=1 pnpm test 后人工复核 *.html,并按 AGENTS.md
// 硬规则补真实 GUI 实操截图归档。
/// <reference types="node" />
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { AddNodeWizard } from "@/components/wizard/AddNodeWizard";
import { CommandPalette } from "@/components/cmdk/CommandPalette";
import { publishPaletteSource } from "@/lib/cmdk/sourceStore";

// Fake* 命名纪律:测试夹具;零真实网络(DoD 禁止行为)。
vi.mock("@/bindings", () => ({
  commands: {
    providersFetch: async () => ({
      status: "ok",
      data: [
        { name: "123pan", description: "123 Cloud Drive", fields: [] },
        { name: "baidu_netdisk", description: "Baidu Netdisk", fields: [] },
        { name: "onedrive", description: "Microsoft OneDrive", fields: [] },
        { name: "smb", description: "SMB over network", fields: [] },
      ],
    }),
  },
}));

// jsdom 缺口:cmdk 依赖 ResizeObserver 与 scrollIntoView,测试环境以空实现桩补齐(仅测试,命名 Fake*)。
class FakeResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
vi.stubGlobal("ResizeObserver", FakeResizeObserver);
Element.prototype.scrollIntoView = function fakeScrollIntoView(): void {};

const ARCHIVE = process.env.GUI_ARCHIVE === "1";
const BANNER = "<!doctype html><html><head><meta charset='utf-8'><title>Unattended render snapshot</title></head><body>"
  + "<!-- 无人值守轮渲染快照(vitest+jsdom):待 Owner GUI 实操验收,非 GUI 验收记录 -->\n";

function archive(fileName: string): void {
  if (!ARCHIVE) return;
  const dir = path.resolve(process.cwd(), "../docs/screenshots");
  mkdirSync(dir, { recursive: true });
  // XMLSerializer 序列化(禁 innerHTML 读取:ESLint 逃逸通道护栏一视同仁)。
  writeFileSync(path.join(dir, fileName), `${BANNER}${new XMLSerializer().serializeToString(document.body)}</body></html>\n`);
}

afterEach(cleanup);

it("向导渲染:provider 四分支分组就绪态(断言+归档)", async () => {
  render(<AddNodeWizard open onClose={() => undefined} />);
  expect(await screen.findByText("123pan")).toBeTruthy();
  expect(screen.getByText("baidu_netdisk")).toBeTruthy();
  expect(screen.getByText("smb")).toBeTruthy();
  archive("wizard-step1.html");
});

it("⌘K 面板渲染:三区齐备→检索过滤生效(断言+归档)", async () => {
  publishPaletteSource({
    nodeLabel: "Local",
    path: "/",
    files: [
      { id: "/alpha.txt", zone: "files", label: "alpha.txt", detail: "1 B" },
      { id: "/report.pdf", zone: "files", label: "report.pdf", detail: "2 B" },
    ],
  });
  render(<CommandPalette open onOpenChange={() => undefined} actions={{ onAddNode: () => undefined, onOpenDiagnostics: () => undefined, onBrowse: () => undefined }} />);
  const input = screen.getByPlaceholderText("Search files, nodes, commands…");
  // 空查询:三区全量(文件×2 + Node×1 + 命令×3)——此态归档。
  expect(screen.getByText("alpha.txt")).toBeTruthy();
  expect(screen.getByText("report.pdf")).toBeTruthy();
  expect(screen.getByText("Local")).toBeTruthy();
  expect(screen.getByText("Add Node")).toBeTruthy();
  archive("cmdk.html");

  // 输入防抖(40ms)后检索生效:仅 alpha.txt 存活。
  fireEvent.change(input, { target: { value: "alpha" } });
  await waitFor(() => expect(screen.queryByText("report.pdf")).toBeNull());
  expect(screen.getByText("alpha.txt")).toBeTruthy();
  expect(screen.queryByText("Add Node")).toBeNull();
});
