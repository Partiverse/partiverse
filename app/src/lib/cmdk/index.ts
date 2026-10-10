// ⌘K 命令面板纯逻辑层(DoD④⑥,零 React/IPC 依赖):三区本地索引(文件/Node/命令)、
// 评分匹配(精确>前缀>词首>子串>子序列)、区内按分数+标签排序、输入防抖。
// 性能预算(08 规划 §8):⌘K 响应 <100ms —— 基准计时断言见 index.test.ts。
export type PaletteZone = "files" | "nodes" | "commands";
/** 三区固定呈现序(08 规划 §4.3:本地索引即时 → 在线 Node → 命令)。 */
export const ZONE_ORDER: readonly PaletteZone[] = ["files", "nodes", "commands"];

export interface PaletteEntry {
  id: string;
  zone: PaletteZone;
  label: string;
  detail: string | null;
}

export interface PaletteIndex {
  entries: PaletteEntry[];
}

/** 构建本地索引;id 重复视为缺陷立即抛错(禁静默吞并)。 */
export function buildIndex(entries: PaletteEntry[]): PaletteIndex {
  const seen = new Set<string>();
  for (const entry of entries) {
    if (seen.has(entry.id)) throw new Error(`duplicate palette entry id: ${entry.id}`);
    seen.add(entry.id);
  }
  return { entries };
}

function isWordChar(ch: string): boolean {
  return /[a-z0-9]/.test(ch);
}

function isSubsequence(text: string, query: string): boolean {
  let at = 0;
  for (const ch of text) {
    if (ch === query[at]) {
      at += 1;
      if (at === query.length) return true;
    }
  }
  return false;
}

/** 相关性评分:越大越相关;null=不匹配。空查询=0(全量、默认序)。 */
export function matchScore(label: string, query: string): number | null {
  const q = query.trim().toLowerCase();
  if (q === "") return 0;
  const l = label.toLowerCase();
  if (l === q) return 100;
  if (l.startsWith(q)) return 80;
  const at = l.indexOf(q);
  if (at > 0 && !isWordChar(l[at - 1])) return 60;
  if (at >= 0) return 40;
  return isSubsequence(l, q) ? 10 : null;
}

/** 检索:三区固定序,区内按 分数降序+标签字典序(稳定可测)。 */
export function search(index: PaletteIndex, query: string): PaletteEntry[] {
  return index.entries
    .map((entry) => ({ entry, score: matchScore(entry.label, query) }))
    .filter((row): row is { entry: PaletteEntry; score: number } => row.score !== null)
    .sort((a, b) =>
      ZONE_ORDER.indexOf(a.entry.zone) - ZONE_ORDER.indexOf(b.entry.zone)
      || b.score - a.score
      || a.entry.label.localeCompare(b.entry.label))
    .map((row) => row.entry);
}

/** 输入防抖窗口:本地索引同步检索,总响应仍远低于 100ms 预算。 */
export const PALETTE_DEBOUNCE_MS = 40;

export interface DebouncedFn {
  (...args: string[]): void;
  cancel: () => void;
}

/** 防抖:窗口内重置计时,仅最后一次生效;cancel 供卸载清理。 */
export function debounce(fn: (...args: string[]) => void, waitMs: number): DebouncedFn {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const wrapped = (...args: string[]): void => {
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      fn(...args);
    }, waitMs);
  };
  wrapped.cancel = (): void => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };
  return wrapped;
}
