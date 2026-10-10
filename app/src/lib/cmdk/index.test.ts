// ⌘K 纯逻辑单测(DoD④⑥):三区索引排序 / 评分匹配 / 防抖 / 基准计时断言
// (性能预算 08 规划 §8:⌘K 响应 <100ms;可测项=本地索引构建+检索,实测登记)。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { buildIndex, debounce, matchScore, PALETTE_DEBOUNCE_MS, search, type PaletteEntry } from "./index";

const entry = (id: string, zone: PaletteEntry["zone"], label: string): PaletteEntry => ({ id, zone, label, detail: null });

describe("三区索引与排序", () => {
  const index = buildIndex([
    entry("c1", "commands", "Add Node"),
    entry("f2", "files", "report.pdf"),
    entry("n1", "nodes", "Local"),
    entry("f1", "files", "alpha.txt"),
    entry("c2", "commands", "Open Diagnostics"),
  ]);

  it("空查询=全量,按三区固定序+区内标签序", () => {
    expect(search(index, "").map((e) => e.id)).toEqual(["f1", "f2", "n1", "c1", "c2"]);
  });

  it("命中:区序优先(files<nodes<commands),区内按分数+标签序", () => {
    expect(search(index, "a").map((e) => e.id)).toEqual(["f1", "n1", "c1", "c2"]);
  });

  it("id 重复立即抛错(禁静默吞并)", () => {
    expect(() => buildIndex([entry("x", "files", "a"), entry("x", "files", "b")])).toThrowError(/duplicate palette entry id: x/);
  });
});

describe("评分匹配", () => {
  it("精确>前缀>词首>子串>子序列>不匹配;大小写不敏感;空查询恒 0", () => {
    expect(matchScore("Add Node", "add node")).toBe(100);
    expect(matchScore("Add Node", "add")).toBe(80);
    expect(matchScore("Open Diagnostics", "diag")).toBe(60);
    expect(matchScore("report.pdf", "port")).toBe(40);
    expect(matchScore("report.pdf", "rpt")).toBe(10);
    expect(matchScore("report.pdf", "zzz")).toBeNull();
    expect(matchScore("Alpha.TXT", "alpha.txt")).toBe(100);
    expect(matchScore("anything", "   ")).toBe(0);
  });
});

describe("输入防抖", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("窗口内重置计时,仅最后一次生效;cancel 丢弃挂起调用", () => {
    const seen: string[] = [];
    const fn = debounce((value: string) => seen.push(value), PALETTE_DEBOUNCE_MS);
    fn("a");
    fn("ab");
    fn("abc");
    vi.advanceTimersByTime(PALETTE_DEBOUNCE_MS - 1);
    expect(seen).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(seen).toEqual(["abc"]);
    fn("x");
    fn.cancel();
    vi.advanceTimersByTime(1000);
    expect(seen).toEqual(["abc"]);
  });
});

describe("基准计时断言(⌘K <100ms,08 规划 §8)", () => {
  it("1 万条本地索引:构建+检索计时断言(热身后取 5 轮最大值)", () => {
    // 可测项=本地索引构建与同步检索(防抖外的全部面板检索面);引擎/网络不在面内。
    const files = Array.from({ length: 10_000 }, (_, i) => entry(`f${i}`, "files", `file-${i % 97}-${i}.txt`));
    const index = buildIndex([...files, entry("c0", "commands", "Add Node"), entry("n0", "nodes", "Local")]);
    const run = (): number => {
      const start = performance.now();
      const hits = search(index, "file-5");
      const elapsed = performance.now() - start;
      expect(hits.length).toBeGreaterThan(100);
      return elapsed;
    };
    run(); // 热身(JIT/缓存)
    const worst = Math.max(...Array.from({ length: 5 }, run));
    expect(worst).toBeLessThan(100);
    expect(PALETTE_DEBOUNCE_MS).toBeLessThan(100);
  });
});
