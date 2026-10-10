// 虚拟化 treegrid(DoD①⑨):TanStack Table v9(features 架构)+ react-virtual。
// 展开压平 = getExpandedRowModel 扁平行模型,虚拟化只渲染视口行(禁全量渲染)。
// ARIA 按 W3C APG treegrid:容器聚焦+aria-activedescendant;键盘走纯函数
// treeGridNav(焦点路径有单测)。
import { useCallback, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { useEffect } from "react";
import {
  createColumnHelper, createExpandedRowModel, FlexRender, rowExpandingFeature,
  tableFeatures, useTable, type ExpandedState, type Updater,
} from "@tanstack/react-table";
import { useVirtualizer } from "@tanstack/react-virtual";
import { t } from "@/i18n";
import { formatBytes, formatMtime, treeGridNav, type BrowseItem, type NavKey } from "@/lib/browse/model";

// v9:静态声明展开 feature+行模型工厂(helper.columns 保序保型)。
const features = tableFeatures({ rowExpandingFeature, expandedRowModel: createExpandedRowModel() });
type Features = typeof features;
const helper = createColumnHelper<Features, BrowseItem>();
const GRID_TEMPLATE = "minmax(0,1fr) 96px 160px 88px 84px";

const columns = helper.columns([
  helper.display({
    id: "name",
    header: t("browse.nameColumn"),
    cell: ({ row }) => {
      const item = row.original;
      if (item.kind === "skeleton") return <span aria-hidden className="inline-block h-3 w-24 animate-pulse rounded bg-muted" />;
      if (item.kind === "error" || item.entry === null) return <span className="truncate text-destructive">{item.message}</span>;
      const entry = item.entry;
      return (
        <span className="flex min-w-0 items-center" style={{ paddingInlineStart: row.depth * 16 }}>
          {row.getCanExpand() ? (
            <button
              type="button"
              aria-label={`${row.getIsExpanded() ? t("browse.collapse") : t("browse.expand")} ${entry.name}`}
              onClick={(event) => {
                event.stopPropagation();
                row.toggleExpanded();
              }}
              className="me-1 inline-flex size-4 shrink-0 items-center justify-center rounded hover:bg-muted"
            >
              <span aria-hidden className="text-[10px]">{row.getIsExpanded() ? "▾" : "▸"}</span>
            </button>
          ) : (
            <span aria-hidden className="me-1 inline-block size-4 shrink-0" />
          )}
          <span className="truncate">{entry.name}</span>
        </span>
      );
    },
  }),
  helper.accessor((item) => item.entry?.sizeBytes ?? null, {
    id: "size", header: t("browse.sizeColumn"),
    cell: (info) => <span className="text-muted-foreground">{formatBytes(info.getValue())}</span>,
  }),
  helper.accessor((item) => item.entry?.mtimeMs ?? null, {
    id: "modified", header: t("browse.modifiedColumn"),
    cell: (info) => <span className="text-muted-foreground">{formatMtime(info.getValue())}</span>,
  }),
  helper.accessor((item) => item.entry?.nodeLabel ?? "", {
    id: "node", header: t("browse.nodeColumn"),
    cell: (info) => <span className="text-muted-foreground">{info.getValue()}</span>,
  }),
  helper.display({
    id: "status",
    header: t("browse.statusColumn"),
    cell: ({ row }) => (
      <span className="text-muted-foreground">
        {t(row.original.kind === "entry" ? "browse.statusReady" : row.original.kind === "skeleton" ? "browse.statusLoading" : "browse.statusError")}
      </span>
    ),
  }),
]);

export interface BrowseTreeGridProps {
  data: BrowseItem[];
  expanded: Record<string, boolean>;
  onExpandedChange: (updater: (old: Record<string, boolean>) => Record<string, boolean>) => void;
  selectedId: string | null;
  onSelect: (item: BrowseItem) => void;
}

export function BrowseTreeGrid({ data, expanded, onExpandedChange, selectedId, onSelect }: BrowseTreeGridProps) {
  // TanStack ExpandedState 允许 true(全展开);本视图无全展开语义,保守折叠为现状映射。
  const handleExpandedChange = useCallback(
    (updater: Updater<ExpandedState>) => {
      onExpandedChange((old) => {
        const next = typeof updater === "function" ? updater(old) : updater;
        return next === true ? { ...old } : next;
      });
    },
    [onExpandedChange],
  );
  const table = useTable({
    features,
    columns,
    data,
    getRowId: (item) => item.id,
    getSubRows: (item) => item.subRows,
    state: { expanded },
    onExpandedChange: handleExpandedChange,
  });
  const rows = table.getExpandedRowModel().rows;

  const scrollRef = useRef<HTMLDivElement | null>(null);
  // react-hooks/incompatible-library:TanStack headless hook 订阅外部 store,React
  // Compiler 不编译该子树(信息级提示,行为不受影响);基座适配后移除豁免。
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 32,
    overscan: 8,
  });

  // 活动行(焦点模型 aria-activedescendant);行集收缩时收敛到合法区间。
  const [activeIndex, setActiveIndex] = useState(0);
  const safeActive = rows.length === 0 ? 0 : Math.min(activeIndex, rows.length - 1);
  useEffect(() => {
    if (rows.length > 0) virtualizer.scrollToIndex(safeActive);
  }, [safeActive, rows.length, virtualizer]);

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const navKeys: readonly string[] = ["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End"];
    if (!navKeys.includes(event.key) && event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    if (event.key === "Enter" || event.key === " ") {
      const row = rows[safeActive];
      if (row !== undefined) onSelect(row.original);
      return;
    }
    const outcome = treeGridNav(
      rows.map((row) => ({ id: row.id, depth: row.depth, canExpand: row.getCanExpand(), expanded: row.getIsExpanded() })),
      safeActive,
      event.key as NavKey,
    );
    setActiveIndex(outcome.index);
    const toggle = outcome.toggle;
    if (toggle !== null) onExpandedChange((old) => ({ ...old, [toggle.id]: toggle.expand }));
  };

  const headerGroup = table.getHeaderGroups()[0];
  const gridTemplate = useMemo(() => ({ gridTemplateColumns: GRID_TEMPLATE }), []);

  return (
    <div
      role="treegrid"
      aria-label={t("browse.treeLabel")}
      aria-rowcount={rows.length + 1}
      aria-colcount={columns.length}
      aria-activedescendant={rows.length > 0 ? `browse-row-${safeActive}` : undefined}
      tabIndex={0}
      onKeyDown={onKeyDown}
      ref={scrollRef}
      className="min-h-0 flex-1 overflow-auto outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
    >
      <div role="rowgroup">
        {headerGroup !== undefined && (
          <div role="row" className="sticky top-0 z-10 grid h-8 items-center border-b bg-background text-xs font-medium text-muted-foreground" style={gridTemplate}>
            {headerGroup.headers.map((header) => (
              <div key={header.id} role="columnheader" className="px-2">
                <FlexRender header={header} />
              </div>
            ))}
          </div>
        )}
      </div>
      <div role="rowgroup" className="relative" style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((virtualRow) => {
          const row = rows[virtualRow.index];
          if (row === undefined) return null;
          const item = row.original;
          return (
            <div
              key={row.id}
              role="row"
              id={`browse-row-${virtualRow.index}`}
              aria-level={row.depth + 1}
              aria-selected={selectedId !== null && selectedId === item.id}
              aria-expanded={row.getCanExpand() ? row.getIsExpanded() : undefined}
              aria-busy={item.kind === "skeleton"}
              onClick={() => onSelect(item)}
              className="absolute inset-x-0 grid h-8 items-center border-b border-border/50 text-sm hover:bg-muted/40 aria-selected:bg-accent"
              style={{ transform: `translateY(${virtualRow.start}px)`, ...gridTemplate }}
            >
              {row.getAllCells().map((cell) => (
                <div key={cell.id} role="gridcell" className="min-w-0 px-2">
                  <FlexRender cell={cell} />
                </div>
              ))}
            </div>
          );
        })}
      </div>
    </div>
  );
}
