// TreeGrid 行为单测(DoD⑨):treegrid ARIA(APG)+ 全键盘焦点路径。
// jsdom 的 offset 尺寸定义在 HTMLElement.prototype(已查证),覆盖之供 virtualizer
// 量测;ResizeObserver 缺失时 virtual-core 自行降级,无需 stub。
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { BrowseTreeGrid } from "./BrowseTreeGrid";
import type { BrowseItem } from "@/lib/browse/model";

beforeEach(() => {
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", { configurable: true, get: () => 480 });
  Object.defineProperty(HTMLElement.prototype, "offsetWidth", { configurable: true, get: () => 800 });
});

afterEach(() => {
  Reflect.deleteProperty(HTMLElement.prototype, "offsetHeight");
  Reflect.deleteProperty(HTMLElement.prototype, "offsetWidth");
  cleanup();
});

function fileItem(name: string): BrowseItem {
  return { kind: "entry", id: `/${name}`, entry: { id: `/${name}`, name, isDir: false, sizeBytes: 10, mtimeMs: 1000, nodeLabel: "Local" }, message: null, subRows: [] };
}

function renderGrid() {
  const data: BrowseItem[] = [
    { kind: "entry", id: "/docs", entry: { id: "/docs", name: "docs", isDir: true, sizeBytes: null, mtimeMs: 1000, nodeLabel: "Local" }, message: null, subRows: [fileItem("a.txt"), fileItem("b.txt")] },
    fileItem("z.txt"),
  ];
  const onExpandedChange = vi.fn();
  const onSelect = vi.fn();
  const utils = render(
    <BrowseTreeGrid data={data} expanded={{ "/docs": true }} onExpandedChange={onExpandedChange} selectedId={null} onSelect={onSelect} />,
  );
  return { onExpandedChange, onSelect, ...utils };
}

describe("BrowseTreeGrid a11y(APG treegrid)", () => {
  it("渲染 treegrid 语义:role/列头/行级与展开态", () => {
    renderGrid();
    expect(screen.getByRole("treegrid")).toBeTruthy();
    expect(screen.getByRole("columnheader", { name: "Name" })).toBeTruthy();
    expect(screen.getByRole("columnheader", { name: "Size" })).toBeTruthy();
    // 展开的目录:子行可见且带 aria-level;目录行 aria-expanded=true。
    expect(screen.getByRole("row", { name: /docs/ }).getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("row", { name: /a\.txt/ }).getAttribute("aria-level")).toBe("2");
    expect(screen.getByRole("row", { name: /z\.txt/ }).getAttribute("aria-level")).toBe("1");
  });

  it("键盘焦点路径:容器持有焦点,↓/↑ 经 aria-activedescendant 移动活动行", () => {
    const { getByRole } = renderGrid();
    const grid = getByRole("treegrid");
    grid.focus();
    expect(document.activeElement).toBe(grid);
    expect(grid.getAttribute("aria-activedescendant")).toBe("browse-row-0");
    fireEvent.keyDown(grid, { key: "ArrowDown" });
    expect(grid.getAttribute("aria-activedescendant")).toBe("browse-row-1");
    fireEvent.keyDown(grid, { key: "ArrowDown" });
    expect(grid.getAttribute("aria-activedescendant")).toBe("browse-row-2");
    fireEvent.keyDown(grid, { key: "ArrowUp" });
    expect(grid.getAttribute("aria-activedescendant")).toBe("browse-row-1");
    expect(document.activeElement).toBe(grid);
  });

  it("Enter 选中活动行;← 折叠已展开目录(受控回调)", () => {
    const { getByRole, onSelect, onExpandedChange } = renderGrid();
    const grid = getByRole("treegrid");
    grid.focus();
    fireEvent.keyDown(grid, { key: "ArrowDown" }); // 活动行 → /docs 下的 a.txt
    fireEvent.keyDown(grid, { key: "Enter" });
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect.mock.calls[0]?.[0]?.id).toBe("/a.txt");

    fireEvent.keyDown(grid, { key: "ArrowUp" }); // 回到 /docs(已展开目录行)
    fireEvent.keyDown(grid, { key: "ArrowLeft" }); // 折叠
    expect(onExpandedChange).toHaveBeenCalledTimes(1);
    const updater = onExpandedChange.mock.calls[0]?.[0] as (old: Record<string, boolean>) => Record<string, boolean>;
    expect(updater({ "/docs": true })).toEqual({ "/docs": false });
  });
});
