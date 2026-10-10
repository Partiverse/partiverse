// 浏览纯逻辑层(DoD⑧,零 React/IPC 依赖):operations/list 防御解析(线格式=
// rclone lsjson 官方文档,字段只读不发明)、拼音+自然排序(§4.2,Intl.Collator
// zh-u-co-pinyin,零新依赖)、前缀筛选 chips、四态状态机、树构建、treegrid 键盘
// 导航(APG)、格式化。

/** 单条目录项(size/mtime 缺失记 null,禁造默认值)。 */
export interface BrowseEntry {
  id: string;
  name: string;
  isDir: boolean;
  sizeBytes: number | null;
  mtimeMs: number | null;
  nodeLabel: string;
}

/** 树行模型(TanStack TData):目录未载入=skeleton 行,列举失败=error 行(禁吞错)。 */
export interface BrowseItem {
  kind: "entry" | "skeleton" | "error";
  id: string;
  entry: BrowseEntry | null;
  message: string | null;
  subRows: BrowseItem[];
}

/** 目录列举缓存;root 状态驱动视图四态。 */
export interface DirState {
  status: "loading" | "ready" | "error";
  entries: BrowseEntry[];
  error: string | null;
}

export type ParseResult = { ok: true; entries: BrowseEntry[] } | { ok: false; error: string };

/** posix 路径拼接(root 特判,避免 "//name")。 */
export function joinPath(parent: string, name: string): string {
  return parent === "/" ? `/${name}` : `${parent}/${name}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 解析 operations/list 载荷(壳透传 JSON 文本);形状不符一律上浮,禁吞成空列表。 */
export function parseDirListing(raw: string, parentPath: string, nodeLabel: string): ParseResult {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return { ok: false, error: "invalid JSON payload from operations/list" };
  }
  if (!isRecord(parsed)) return { ok: false, error: "unexpected payload shape (not an object)" };
  const list = parsed["list"];
  if (list !== undefined && !Array.isArray(list)) {
    return { ok: false, error: 'unexpected "list" field (not an array)' };
  }
  const entries: BrowseEntry[] = [];
  for (const item of list ?? []) {
    if (!isRecord(item)) return { ok: false, error: "list item is not an object" };
    const { Name: name, IsDir: isDir } = item as { Name?: unknown; IsDir?: unknown };
    if (typeof name !== "string" || typeof isDir !== "boolean") {
      return { ok: false, error: "list item missing Name:string / IsDir:boolean" };
    }
    const size = item["Size"];
    const modTime = item["ModTime"];
    const mtimeMs = typeof modTime === "string" && modTime !== "" ? Date.parse(modTime) : null;
    entries.push({
      id: joinPath(parentPath, name),
      name,
      isDir,
      sizeBytes: typeof size === "number" ? size : null,
      mtimeMs: mtimeMs !== null && !Number.isNaN(mtimeMs) ? mtimeMs : null,
      nodeLabel,
    });
  }
  return { ok: true, entries };
}

// ── 排序:拼音+自然(numeric),中英文同一比较器→方向恒一致;主键同值回退名称。──
export type SortKey = "name" | "size" | "mtime";
export interface SortSpec {
  key: SortKey;
  desc: boolean;
}
/** 默认=修改时间倒序(§4.2)。 */
export const DEFAULT_SORT: SortSpec = { key: "mtime", desc: true };

const pinyinCollator = new Intl.Collator(["zh-u-co-pinyin", "en"], { numeric: true });

/** 名称比较(拼音+自然,file2<file10)。 */
export function compareByName(a: string, b: string): number {
  return pinyinCollator.compare(a, b);
}

/** 行比较:null 恒殿后(不受方向影响);desc 反转非空主键(名称键同受控);
 * size/mtime 主键同值回退名称(回退段恒升序,不随方向)。 */
export function compareEntries(a: BrowseEntry, b: BrowseEntry, spec: SortSpec): number {
  if (spec.key === "name") {
    // 名称主键:拼音+自然序的 base 结果同样按 desc 反转(修复:方向开关对名称键失效)。
    const base = compareByName(a.name, b.name);
    return base === 0 ? 0 : spec.desc ? -base : base;
  }
  const av = spec.key === "size" ? a.sizeBytes : a.mtimeMs;
  const bv = spec.key === "size" ? b.sizeBytes : b.mtimeMs;
  if (av === null || bv === null) {
    if (av !== null || bv !== null) return av === null ? 1 : -1;
  } else if (av !== bv) {
    return spec.desc === (av < bv) ? 1 : -1;
  }
  return compareByName(a.name, b.name); // 主键同值/null 同态:回退名称恒升序
}

// ── 筛选:前缀 chips(多 chip OR、大小写不敏感;空集=不过滤)──
export function normalizeChip(raw: string): string | null {
  const trimmed = raw.trim();
  return trimmed === "" ? null : trimmed;
}

export function matchesChips(name: string, chips: readonly string[]): boolean {
  if (chips.length === 0) return true;
  const lower = name.toLowerCase();
  return chips.some((chip) => lower.startsWith(chip.toLowerCase()));
}

// ── 四态状态机(§5:空/骨架/错误可重试/成功;缓存壳不回退骨架)──
export type BrowsePhase = "skeleton" | "empty" | "error" | "success";

/** root 目录状态 → 视图四态(纯推导)。 */
export function deriveBrowsePhase(root: DirState | undefined): BrowsePhase {
  if (root === undefined || root.status === "loading") return "skeleton";
  if (root.status === "error") return "error";
  return root.entries.length === 0 ? "empty" : "success";
}

// ── 树构建:缓存+展开态 → 层级数据(压平交给 TanStack getExpandedRowModel);
// nodeLabel 已随解析写入 entry,无需再传。──
/** 构建根树:root 未载入 → 整树骨架行(慢源渐进,禁空白沉默)。 */
export function buildTreeData(
  rootPath: string,
  dirs: Readonly<Record<string, DirState>>,
  expanded: Readonly<Record<string, boolean>>,
  sort: SortSpec,
  chips: readonly string[],
): BrowseItem[] {
  const skeleton = (id: string, message: string | null): BrowseItem =>
    message === null
      ? { kind: "skeleton", id, entry: null, message: null, subRows: [] }
      : { kind: "error", id, entry: null, message, subRows: [] };
  const build = (dirPath: string): BrowseItem[] => {
    const dir = dirs[dirPath];
    if (dir === undefined) return [skeleton(`${dirPath}::loading`, null)];
    if (dir.status === "loading") return [skeleton(`${dirPath}::loading`, null)];
    if (dir.status === "error") return [skeleton(`${dirPath}::error`, dir.error)];
    return dir.entries
      .filter((entry) => matchesChips(entry.name, chips))
      .sort((a, b) => compareEntries(a, b, sort))
      .map((entry) => ({
        kind: "entry" as const,
        id: entry.id,
        entry,
        message: null,
        subRows: entry.isDir && expanded[entry.id] === true ? build(entry.id) : [],
      }));
  };
  return build(rootPath);
}

// ── treegrid 键盘导航(APG 子集:↑↓ 行移动,←→ 折叠/展开或跳父,Home/End)──
export interface NavRow {
  id: string;
  depth: number;
  canExpand: boolean;
  expanded: boolean;
}
export interface NavOutcome {
  index: number;
  toggle: { id: string; expand: boolean } | null;
}
export type NavKey = "ArrowDown" | "ArrowUp" | "ArrowLeft" | "ArrowRight" | "Home" | "End";

/** 焦点路径纯函数:当前活动行+按键 → 下一活动行(及展开切换),供单测钉住。 */
export function treeGridNav(rows: readonly NavRow[], active: number, key: NavKey): NavOutcome {
  const last = rows.length - 1;
  const clamp = (i: number) => Math.max(0, Math.min(last, i));
  const none: NavOutcome = { index: clamp(active), toggle: null };
  if (rows.length === 0) return none;
  const row = rows[clamp(active)];
  switch (key) {
    case "ArrowDown":
      return { index: clamp(active + 1), toggle: null };
    case "ArrowUp":
      return { index: clamp(active - 1), toggle: null };
    case "Home":
      return { index: 0, toggle: null };
    case "End":
      return { index: last, toggle: null };
    case "ArrowRight":
      if (row.canExpand && !row.expanded) return { index: active, toggle: { id: row.id, expand: true } };
      return { index: clamp(active + 1), toggle: null };
    case "ArrowLeft":
      if (row.canExpand && row.expanded) return { index: active, toggle: { id: row.id, expand: false } };
      for (let i = active - 1; i >= 0; i -= 1) {
        if (rows[i].depth < row.depth) return { index: i, toggle: null }; // 最近可展开祖先
      }
      return { index: active, toggle: null };
  }
}

// ── 展示格式化(GUI 英文;null 显式 "—" 而非空白)──
const mtimeFormat = new Intl.DateTimeFormat("en-US", {
  year: "numeric", month: "short", day: "2-digit", hour: "2-digit", minute: "2-digit",
});

export function formatBytes(size: number | null): string {
  if (size === null) return "—";
  let value = size;
  let unit = "B";
  for (const next of ["KiB", "MiB", "GiB", "TiB"]) {
    if (Math.abs(value) < 1024) break;
    value /= 1024;
    unit = next;
  }
  return unit === "B" ? `${value} B` : `${value.toFixed(1)} ${unit}`;
}

export function formatMtime(ms: number | null): string {
  return ms === null ? "—" : mtimeFormat.format(new Date(ms));
}
