// 浏览数据接线(DoD①④):消费 T01 bindings.operationsList,目录逐层列举;
// 缓存壳优先(已列目录秒开)、未载目录骨架占位(慢源渐进)。命令信封错误与
// 传输层异常一律上浮为 DirState.error 并记入健康徽标「最近错误」,禁吞错。
// 当前节点=本地盘(rclone local 后端,fs=裸路径;远端 Node 枚举命令未交付,
// 后续 WP 接入)。M1-WP05-T09:operations_list 两键齐传(remote 空串合法,
// 仅 fs 单键 = 引擎 400,实测锚定);默认根=壳解析的用户家目录(DoD③,
// 修复 F3 浏览默认根=/ 与新建文件夹 permission denied)。
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { commands, type CmdError } from "@/bindings";
import { recordEngineError } from "@/lib/healthStore";
import { publishPaletteSource } from "@/lib/cmdk/sourceStore";
import { formatBytes } from "@/lib/browse/model";
import {
  buildTreeData, compareEntries, DEFAULT_SORT, deriveBrowsePhase,
  matchesChips, normalizeChip, parseDirListing,
  type BrowseEntry, type BrowseItem, type BrowsePhase, type DirState, type SortSpec,
} from "@/lib/browse/model";
import { t } from "@/i18n";

export interface BrowseNode {
  label: string;
}
export const LOCAL_NODE: BrowseNode = { label: "Local" };

function fsForPath(path: string): string {
  return path; // local 后端 fs 即裸路径(remote:path 的本地形态)
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : JSON.stringify(err);
}

function envelopeError(cmdError: CmdError): string {
  return `${cmdError.kind}: ${cmdError.msg}`;
}

export interface BrowseController {
  rootPath: string;
  phase: BrowsePhase;
  rootError: string | null;
  data: BrowseItem[];
  rootEntries: BrowseEntry[];
  sort: SortSpec;
  chips: string[];
  expanded: Record<string, boolean>;
  setRootPath: (path: string) => void;
  setSort: (spec: SortSpec) => void;
  addChip: (raw: string) => void;
  removeChip: (index: number) => void;
  onExpandedChange: (updater: (old: Record<string, boolean>) => Record<string, boolean>) => void;
  retry: () => void;
}

export function useBrowse(node: BrowseNode): BrowseController {
  // 默认根(DoD③):null = 家目录解析中(骨架态);解析失败 = homeError 上浮
  // (错误态,禁回退 "/"——权限缺陷不许藏成 UX 问题)。
  const [rootPath, setRootPath] = useState<string | null>(null);
  const [homeError, setHomeError] = useState<string | null>(null);
  const [dirs, setDirs] = useState<Record<string, DirState>>({});
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [sort, setSort] = useState<SortSpec>(DEFAULT_SORT);
  const [chips, setChips] = useState<string[]>([]);

  // 家目录解析(壳 user_home_dir,HOME/USERPROFILE 零硬编码):仅挂载时一次。
  useEffect(() => {
    let cancelled = false;
    commands
      .userHomeDir()
      .then((result) => {
        if (cancelled) return;
        if (result.status === "error") {
          const message = envelopeError(result.error);
          recordEngineError(message);
          setHomeError(message);
        } else {
          setRootPath(result.data);
        }
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        const message = errorText(err);
        recordEngineError(message);
        setHomeError(message);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const dirsRef = useRef(dirs);
  useEffect(() => {
    dirsRef.current = dirs;
  }, [dirs]);
  const engineEnsuredRef = useRef(false);

  const loadDir = useCallback(
    async (path: string) => {
      setDirs((prev) => ({ ...prev, [path]: { status: "loading", entries: [], error: null } }));
      try {
        // 冷路径:确保引擎槽位就绪(幂等);失败=引擎面上浮。
        if (!engineEnsuredRef.current) {
          const ensured = await commands.engineEnsure(null);
          if (ensured.status === "error") throw new Error(envelopeError(ensured.error));
          engineEnsuredRef.current = true;
        }
        // 两键齐传(remote 空串合法):仅 fs 单键 = 引擎 400(T09 实测锚定)。
        const result = await commands.operationsList(fsForPath(path), "");
        if (result.status === "error") throw new Error(envelopeError(result.error));
        const parsed = parseDirListing(result.data, path, node.label);
        if (!parsed.ok) throw new Error(parsed.error);
        // T03 ⌘K「文件」区数据源:列举成功即发布当前目录条目(仅文件名/大小元数据)。
        publishPaletteSource({
          nodeLabel: node.label,
          path,
          files: parsed.entries.map((entry) => ({
            id: entry.id, zone: "files" as const, label: entry.name,
            detail: entry.isDir ? t("browse.directory") : formatBytes(entry.sizeBytes),
          })),
        });
        setDirs((prev) => ({ ...prev, [path]: { status: "ready", entries: parsed.entries, error: null } }));
      } catch (err) {
        const message = errorText(err);
        recordEngineError(message);
        setDirs((prev) => ({ ...prev, [path]: { status: "error", entries: [], error: message } }));
      }
    },
    [node.label],
  );

  // 根路径变化:缓存命中零触达;未列过才发起(缓存壳,不空白回退);
  // 家目录未解析(null)前零触达。
  useEffect(() => {
    if (rootPath !== null && dirsRef.current[rootPath] === undefined) void loadDir(rootPath);
  }, [rootPath, loadDir]);

  const onExpandedChange = useCallback(
    (updater: (old: Record<string, boolean>) => Record<string, boolean>) => {
      setExpanded((old) => {
        const next = updater(old);
        // 新展开且未列过的目录:立即逐层拉取(展开模型跑 operations/list)。
        for (const [id, open] of Object.entries(next)) {
          if (open && old[id] !== true && dirsRef.current[id] === undefined) void loadDir(id);
        }
        return next;
      });
    },
    [loadDir],
  );

  const addChip = useCallback((raw: string) => {
    const chip = normalizeChip(raw);
    if (chip !== null) setChips((prev) => (prev.includes(chip) ? prev : [...prev, chip]));
  }, []);

  const removeChip = useCallback((index: number) => {
    setChips((prev) => prev.filter((_, i) => i !== index));
  }, []);

  const retry = useCallback(() => {
    if (rootPath !== null) void loadDir(rootPath);
  }, [loadDir, rootPath]);

  // 家目录未解析期间以空根占位构建(空树零行,phase=骨架面接管,不渲染)。
  const resolvedRoot = rootPath ?? "";
  const data = useMemo(
    () => buildTreeData(resolvedRoot, dirs, expanded, sort, chips),
    [resolvedRoot, dirs, expanded, sort, chips],
  );
  const rootEntries = useMemo(() => {
    if (rootPath === null) return [];
    const state = dirs[rootPath];
    if (state === undefined || state.status !== "ready") return [];
    return state.entries.filter((e) => matchesChips(e.name, chips)).sort((a, b) => compareEntries(a, b, sort));
  }, [dirs, rootPath, chips, sort]);

  return {
    rootPath: resolvedRoot,
    phase: homeError !== null ? "error" : deriveBrowsePhase(rootPath === null ? undefined : dirs[rootPath]),
    rootError: homeError ?? (rootPath !== null ? dirs[rootPath]?.error ?? null : null),
    data,
    rootEntries,
    sort,
    chips,
    expanded,
    setRootPath,
    setSort,
    addChip,
    removeChip,
    onExpandedChange,
    retry,
  };
}
