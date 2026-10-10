// 浏览纯逻辑单测(DoD⑧):排序比较器(拼音+自然)/筛选 chips/四态状态机/线格式
// 防御解析/树构建/键盘导航焦点路径/扁平行模型(TanStack expanded row model,
// 官方 vanilla 模式:constructTable + storeReactivityBindings)。
import { describe, expect, it } from "vitest";
import { constructTable, createExpandedRowModel, rowExpandingFeature, tableFeatures } from "@tanstack/react-table";
import { storeReactivityBindings } from "@tanstack/table-core/store-reactivity-bindings";
import {
  buildTreeData, compareByName, compareEntries, DEFAULT_SORT, deriveBrowsePhase,
  formatBytes, formatMtime, joinPath, matchesChips, normalizeChip, parseDirListing,
  treeGridNav, type BrowseEntry, type DirState,
} from "./model";

function entry(name: string, over: Partial<BrowseEntry> = {}): BrowseEntry {
  return { id: joinPath("/", name), name, isDir: false, sizeBytes: 1, mtimeMs: 100, nodeLabel: "Local", ...over };
}

describe("排序比较器(拼音+自然,§4.2)", () => {
  it("自然排序 file2<file10;中文按拼音", () => {
    expect(compareByName("file2.txt", "file10.txt")).toBeLessThan(0);
    expect(compareByName("吧.txt", "文档.txt")).toBeLessThan(0);
    expect(compareByName("文档.txt", "中.txt")).toBeLessThan(0);
  });

  it("中英文方向一致:desc 恰为 asc 逆序", () => {
    const names = ["file10", "file2", "apple", "Apple", "中", "文档", "吧"];
    const asc = names.slice().sort(compareByName);
    expect(names.slice().sort((a, b) => compareByName(b, a))).toEqual(asc.slice().reverse());
  });

  it("名称键 desc 反转(DoD②:方向契约对 name 主键同样生效)", () => {
    const a = entry("a.txt");
    const b = entry("b.txt");
    expect(compareEntries(a, b, { key: "name", desc: false })).toBeLessThan(0);
    expect(compareEntries(b, a, { key: "name", desc: false })).toBeGreaterThan(0);
    expect(compareEntries(a, b, { key: "name", desc: true })).toBeGreaterThan(0);
    expect(compareEntries(b, a, { key: "name", desc: true })).toBeLessThan(0);
  });

  it("默认=修改时间倒序;null 恒殿后(双向)", () => {
    expect(DEFAULT_SORT).toEqual({ key: "mtime", desc: true });
    const newer = entry("a", { mtimeMs: 20 });
    const older = entry("b", { mtimeMs: 10 });
    expect(compareEntries(older, newer, DEFAULT_SORT)).toBeGreaterThan(0); // desc:新的在前
    const known = entry("a", { sizeBytes: 5 });
    const unknown = entry("b", { sizeBytes: null });
    expect(compareEntries(known, unknown, { key: "size", desc: false })).toBeLessThan(0);
    expect(compareEntries(known, unknown, { key: "size", desc: true })).toBeLessThan(0);
  });
});

describe("筛选 chips(前缀 OR,大小写不敏感)", () => {
  it("空集=不过滤;命中与不命中;normalizeChip", () => {
    expect(matchesChips("anything", [])).toBe(true);
    expect(matchesChips("Report.pdf", ["rep"])).toBe(true);
    expect(matchesChips("Report.pdf", ["photo", "re"])).toBe(true);
    expect(matchesChips("Report.pdf", ["photo"])).toBe(false);
    expect(normalizeChip("  doc ")).toBe("doc");
    expect(normalizeChip("   ")).toBeNull();
  });
});

describe("四态状态机(§5)", () => {
  it("骨架/错误/空/成功 全覆盖", () => {
    expect(deriveBrowsePhase(undefined)).toBe("skeleton");
    expect(deriveBrowsePhase({ status: "loading", entries: [], error: null })).toBe("skeleton");
    expect(deriveBrowsePhase({ status: "error", entries: [], error: "boom" })).toBe("error");
    expect(deriveBrowsePhase({ status: "ready", entries: [], error: null })).toBe("empty");
    expect(deriveBrowsePhase({ status: "ready", entries: [entry("a")], error: null })).toBe("success");
  });
});

describe("operations/list 防御解析(离线 fixture)", () => {
  const fixture = JSON.stringify({
    list: [
      { Name: "docs", Path: "docs", IsDir: true, Size: -1, ModTime: "2026-10-01T10:00:00Z" },
      { Name: "a.txt", IsDir: false, Size: 3, ModTime: "" },
    ],
  });

  it("合法载荷 → 归一条目(空 ModTime/缺 Size → null)", () => {
    const parsed = parseDirListing(fixture, "/", "Local");
    expect(parsed.ok).toBe(true);
    if (!parsed.ok) return;
    expect(parsed.entries).toEqual([
      { id: "/docs", name: "docs", isDir: true, sizeBytes: -1, mtimeMs: Date.parse("2026-10-01T10:00:00Z"), nodeLabel: "Local" },
      { id: "/a.txt", name: "a.txt", isDir: false, sizeBytes: 3, mtimeMs: null, nodeLabel: "Local" },
    ]);
  });

  it("子目录前缀拼接;畸形载荷一律上浮(禁静默空列表)", () => {
    const sub = parseDirListing(JSON.stringify({ list: [{ Name: "x", IsDir: false }] }), "/docs", "Local");
    expect(sub.ok && sub.entries[0]?.id).toBe("/docs/x");
    expect(parseDirListing("not-json", "/", "Local").ok).toBe(false);
    expect(parseDirListing("[]", "/", "Local").ok).toBe(false);
    expect(parseDirListing(JSON.stringify({ list: "oops" }), "/", "Local").ok).toBe(false);
    expect(parseDirListing(JSON.stringify({ list: [{ Name: "x" }] }), "/", "Local").ok).toBe(false);
  });
});

describe("树数据构建(展开模型)", () => {
  const dirs: Record<string, DirState> = {
    "/": { status: "ready", entries: [entry("b.txt", { id: "/b.txt" }), entry("zz-dir", { id: "/zz-dir", isDir: true, sizeBytes: null })], error: null },
    "/zz-dir": { status: "loading", entries: [], error: null },
  };

  it("展开中 → 骨架子行;未展开 → 无子行;排序+chips 逐层生效", () => {
    const collapsed = buildTreeData("/", dirs, {}, DEFAULT_SORT, []);
    expect(collapsed.map((item) => item.id)).toEqual(["/b.txt", "/zz-dir"]);
    expect(collapsed[1]?.subRows).toEqual([]);
    expect(buildTreeData("/", dirs, { "/zz-dir": true }, DEFAULT_SORT, [])[1]?.subRows[0]?.kind).toBe("skeleton");
    const multi: Record<string, DirState> = { "/": { status: "ready", entries: [entry("b.txt"), entry("a.txt"), entry("doc.txt")], error: null } };
    expect(buildTreeData("/", multi, {}, { key: "name", desc: false }, []).map((i) => i.id)).toEqual(["/a.txt", "/b.txt", "/doc.txt"]);
    expect(buildTreeData("/", multi, {}, { key: "name", desc: true }, []).map((i) => i.id)).toEqual(["/doc.txt", "/b.txt", "/a.txt"]);
    expect(buildTreeData("/", multi, {}, { key: "name", desc: false }, ["doc"]).map((i) => i.id)).toEqual(["/doc.txt"]);
  });
});

describe("扁平行模型(TanStack expanded row model,vanilla)", () => {
  it("展开子行按深度压平;折叠不出现", () => {
    const features = tableFeatures({
      coreReactivityFeature: storeReactivityBindings(),
      rowExpandingFeature,
      expandedRowModel: createExpandedRowModel(),
    });
    type Item = { id: string; subRows: Item[] };
    const table = constructTable({
      features,
      data: [
        { id: "a", subRows: [{ id: "a1", subRows: [] }, { id: "a2", subRows: [] }] },
        { id: "b", subRows: [{ id: "b1", subRows: [] }] },
      ],
      columns: [{ id: "name", accessorFn: (item: Item) => item.id }],
      getRowId: (item: Item) => item.id,
      getSubRows: (item: Item) => item.subRows,
      state: { expanded: { a: true } },
    });
    const rows = table.getExpandedRowModel().rows;
    expect(rows.map((row) => `${row.depth}:${row.id}`)).toEqual(["0:a", "1:a1", "1:a2", "0:b"]);
    expect(rows[1]?.getCanExpand()).toBe(false);
    expect(rows[0]?.getIsExpanded()).toBe(true);
  });
});

describe("treegrid 键盘导航(焦点路径)", () => {
  const rows = [
    { id: "a", depth: 0, canExpand: true, expanded: true },
    { id: "a1", depth: 1, canExpand: false, expanded: false },
    { id: "a2", depth: 1, canExpand: false, expanded: false },
    { id: "b", depth: 0, canExpand: true, expanded: false },
  ];

  it("↓/↑ 逐行+边界收敛;Home/End 跳首尾", () => {
    expect(treeGridNav(rows, 0, "ArrowDown").index).toBe(1);
    expect(treeGridNav(rows, 3, "ArrowDown").index).toBe(3);
    expect(treeGridNav(rows, 0, "ArrowUp").index).toBe(0);
    expect(treeGridNav(rows, 2, "Home").index).toBe(0);
    expect(treeGridNav(rows, 0, "End").index).toBe(3);
  });

  it("→ 展开折叠行;← 折叠已展开/跳父;空行集不崩溃", () => {
    expect(treeGridNav(rows, 3, "ArrowRight")).toEqual({ index: 3, toggle: { id: "b", expand: true } });
    expect(treeGridNav(rows, 0, "ArrowRight")).toEqual({ index: 1, toggle: null });
    expect(treeGridNav(rows, 0, "ArrowLeft")).toEqual({ index: 0, toggle: { id: "a", expand: false } });
    expect(treeGridNav(rows, 1, "ArrowLeft")).toEqual({ index: 0, toggle: null });
    expect(treeGridNav(rows, 3, "ArrowLeft")).toEqual({ index: 3, toggle: null });
    expect(treeGridNav([], 0, "ArrowDown")).toEqual({ index: 0, toggle: null });
  });
});

describe("展示格式化", () => {
  it("null 显式占位;字节/时间人类可读", () => {
    expect(formatBytes(null)).toBe("—");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KiB");
    expect(formatMtime(null)).toBe("—");
    expect(formatMtime(Date.parse("2026-10-01T00:00:00Z"))).not.toBe("—");
  });
});
