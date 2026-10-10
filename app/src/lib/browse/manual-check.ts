// 手测脚本(M1-WP05-T02:operations_list 数据面离线自检,无需 Tauri/引擎)。
// 运行:node src/lib/browse/manual-check.ts(Node ≥23.6 原生 TS 类型剥离)。
// 离线 fixture 走完整纯逻辑管线:解析→排序(默认修改时间倒序)→chips→树构建
// (展开+骨架)→四态推导→键盘导航。真实端到端(引擎 rc)需在桌面壳内手测。
import {
  buildTreeData, compareEntries, DEFAULT_SORT, deriveBrowsePhase, formatBytes,
  formatMtime, matchesChips, parseDirListing, treeGridNav, type DirState,
} from "./model.ts";

const payload = JSON.stringify({
  list: [
    { Name: "照片", Path: "照片", IsDir: true, Size: -1, ModTime: "2026-10-09T08:00:00Z" },
    { Name: "file10.bin", IsDir: false, Size: 2048, ModTime: "2026-10-01T08:00:00Z" },
    { Name: "file2.bin", IsDir: false, Size: 512, ModTime: "2026-10-05T08:00:00Z" },
    { Name: "readme.md", IsDir: false, Size: 96, ModTime: "2026-10-10T08:00:00Z" },
  ],
});
const parsed = parseDirListing(payload, "/", "Local");
if (!parsed.ok) throw new Error(`PARSE FAILED: ${parsed.error}`); // 失败即终止,兼供 tsc 收窄
const dirs: Record<string, DirState> = {
  "/": { status: "ready", entries: parsed.entries, error: null },
  "/照片": { status: "loading", entries: [], error: null },
};

console.log("== 1) 默认排序(修改时间倒序,null 殿后) ==");
for (const entry of parsed.entries.slice().sort((a, b) => compareEntries(a, b, DEFAULT_SORT))) {
  console.log(`  ${formatMtime(entry.mtimeMs)}  ${formatBytes(entry.sizeBytes).padStart(9)}  ${entry.name}`);
}
console.log("== 2) 名称升序(拼音+自然,file2 < file10,中英文同向) ==");
for (const entry of parsed.entries.slice().sort((a, b) => compareEntries(a, b, { key: "name", desc: false }))) {
  console.log(`  ${entry.name}`);
}
console.log("== 3) chips 前缀筛选 'file' ==");
for (const entry of parsed.entries.filter((item) => matchesChips(item.name, ["file"]))) {
  console.log(`  ${entry.name}`);
}
console.log("== 4) 树构建(展开 照片 → 骨架子行;四态推导) ==");
for (const item of buildTreeData("/", dirs, { "/照片": true }, DEFAULT_SORT, [])) {
  console.log(`  ${item.id}${item.subRows.length > 0 ? ` (+${item.subRows.length} ${item.subRows[0]?.kind})` : ""}`);
}
console.log(`  phase = ${deriveBrowsePhase(dirs["/"])}`);
console.log("== 5) 键盘焦点路径 ↓ ↓ Home → ==");
const navRows = buildTreeData("/", dirs, { "/照片": true }, DEFAULT_SORT, []).map((item) => ({
  id: item.id, depth: 0, canExpand: item.entry?.isDir === true, expanded: item.id === "/照片",
}));
let active = 0;
for (const key of ["ArrowDown", "ArrowDown", "Home", "ArrowRight"] as const) {
  const outcome = treeGridNav(navRows, active, key);
  active = outcome.index;
  console.log(`  ${key} → row ${active} (${navRows[active]?.id})${outcome.toggle ? ` toggle=${JSON.stringify(outcome.toggle)}` : ""}`);
}
