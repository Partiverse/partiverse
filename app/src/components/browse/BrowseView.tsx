// 浏览视图编排(DoD①②③④⑥):面包屑+排序(默认修改时间倒序)+前缀筛选 chips+
// 列表/网格两态+四态框架(空/骨架/错误可重试/成功,禁空白沉默)+可收起详情面板
// 骨架(Particle/驻留文案预留)。M1-WP06-T02 接线:操作行(新建目录/传输/删除)
// + 预览-提交/确认对话框(破坏性操作禁直通;传输经传输 store,重试元数据随
// store 存活)。跨 Node 语境话术=「整理/备份」(合规红线,禁「迁移/搬运」)。
import { useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { commands } from "@/bindings";
import { Button } from "@/components/ui/button";
import { BrowseTreeGrid } from "@/components/browse/BrowseTreeGrid";
import { DeleteConfirm, FolderDialog, TransferDialog } from "@/components/browse/TransferDialog";
import { LOCAL_NODE, useBrowse } from "@/lib/browse/useBrowse";
import { formatBytes, formatMtime, type BrowseEntry, type SortKey } from "@/lib/browse/model";
import { composeFs } from "@/lib/transfer/model";
import { useTransfers } from "@/lib/transfer/useTransfers";
import type { TransferEntry } from "@/lib/transfer/model";
import { t } from "@/i18n";

const SORT_KEYS: SortKey[] = ["name", "size", "mtime"];
const SORT_LABEL_KEY: Record<SortKey, string> = { name: "browse.sortName", size: "browse.sortSize", mtime: "browse.sortModified" };

function Breadcrumb({ rootPath, nodeLabel, onNavigate }: { rootPath: string; nodeLabel: string; onNavigate: (path: string) => void }) {
  const segments = rootPath.split("/").filter((part) => part !== "");
  return (
    <nav aria-label={t("browse.breadcrumbLabel")} className="flex min-w-0 items-center gap-1 text-sm">
      <button type="button" className="rounded px-1 hover:bg-muted" onClick={() => onNavigate("/")}>{nodeLabel}</button>
      {segments.map((segment, index) => {
        const path = `/${segments.slice(0, index + 1).join("/")}`;
        return (
          <span key={path} className="flex min-w-0 items-center gap-1">
            <span aria-hidden className="text-muted-foreground">/</span>
            <button
              type="button"
              aria-current={index === segments.length - 1 ? "location" : undefined}
              className={`truncate rounded px-1 hover:bg-muted ${index === segments.length - 1 ? "font-medium" : ""}`}
              onClick={() => onNavigate(path)}
            >
              {segment}
            </button>
          </span>
        );
      })}
    </nav>
  );
}

/** 网格两态(DoD①):4 列 lanes 虚拟化(仅渲染视口行,禁全量渲染)。 */
function BrowseGrid({ entries, selectedId, onSelect, onOpenDir }: {
  entries: BrowseEntry[];
  selectedId: string | null;
  onSelect: (entry: BrowseEntry) => void;
  onOpenDir: (path: string) => void;
}) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  // react-hooks/incompatible-library:同 BrowseTreeGrid(TanStack headless hook 提示)。
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: entries.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 88,
    lanes: 4,
    overscan: 8,
  });
  return (
    <div ref={scrollRef} role="list" aria-label={t("browse.gridLabel")} className="min-h-0 flex-1 overflow-auto p-2">
      <div className="relative" style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((item) => {
          const entry = entries[item.index];
          if (entry === undefined) return null;
          return (
            <button
              key={entry.id}
              type="button"
              role="listitem"
              aria-selected={selectedId === entry.id}
              onClick={() => (entry.isDir ? onOpenDir(entry.id) : onSelect(entry))}
              className={`absolute m-1 flex w-[calc(25%-8px)] flex-col items-start gap-1 rounded-lg border p-2 text-left text-sm hover:bg-muted/50 aria-selected:border-brand-500 aria-selected:bg-accent ${entry.isDir ? "font-medium" : ""}`}
              style={{ height: 80, transform: `translateY(${item.start}px)`, left: `${(item.lane * 100) / 4}%` }}
            >
              <span className="w-full truncate">{entry.name}</span>
              <span className="text-xs text-muted-foreground">
                {entry.isDir ? t("browse.directory") : formatBytes(entry.sizeBytes)} · {formatMtime(entry.mtimeMs)}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

/** 详情面板骨架(DoD⑥):可收起;Particle/驻留(Placement)文案预留。 */
function DetailPanel({ entry, open, onToggle }: { entry: BrowseEntry | null; open: boolean; onToggle: () => void }) {
  return (
    <aside className="flex w-64 shrink-0 flex-col border-l">
      <div className="flex h-10 items-center justify-between border-b px-3">
        <span className="text-sm font-medium">{t("browse.detailsTitle")}</span>
        <Button variant="ghost" size="icon-xs" aria-expanded={open} aria-controls="browse-details" onClick={onToggle}>
          <span aria-hidden>{open ? "▸" : "◂"}</span>
          <span className="sr-only">{open ? t("browse.detailsCollapse") : t("browse.detailsExpand")}</span>
        </Button>
      </div>
      {open && (
        <dl id="browse-details" className="flex flex-col gap-2 overflow-y-auto p-3 text-sm">
          {entry === null ? (
            <p className="text-muted-foreground">{t("browse.detailsEmpty")}</p>
          ) : (
            <>
              <div><dt className="text-xs text-muted-foreground">{t("browse.nameColumn")}</dt><dd className="truncate">{entry.name}</dd></div>
              <div><dt className="text-xs text-muted-foreground">{t("browse.sizeColumn")}</dt><dd>{entry.isDir ? t("browse.directory") : formatBytes(entry.sizeBytes)}</dd></div>
              <div><dt className="text-xs text-muted-foreground">{t("browse.modifiedColumn")}</dt><dd>{formatMtime(entry.mtimeMs)}</dd></div>
              <div><dt className="text-xs text-muted-foreground">{t("browse.nodeColumn")}</dt><dd>{entry.nodeLabel}</dd></div>
              <div className="mt-2 border-t pt-2">
                <dt className="text-xs text-muted-foreground">Particle</dt>
                <dd className="text-muted-foreground">{t("browse.particleReserved")}</dd>
              </div>
              <div>
                <dt className="text-xs text-muted-foreground">Placements</dt>
                <dd className="text-muted-foreground">{t("browse.placementsReserved")}</dd>
              </div>
            </>
          )}
        </dl>
      )}
    </aside>
  );
}

export function BrowseView() {
  const browse = useBrowse(LOCAL_NODE);
  const transfers = useTransfers();
  const [view, setView] = useState<"list" | "grid">("list");
  const [detailsOpen, setDetailsOpen] = useState(true);
  const [selected, setSelected] = useState<BrowseEntry | null>(null);
  const [chipDraft, setChipDraft] = useState("");
  // T02 操作面:传输(预览-提交)/新建目录/删除确认三对话框互斥开合。
  const [transferOpen, setTransferOpen] = useState(false);
  const [folderOpen, setFolderOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);

  const toEntry = (entry: BrowseEntry): TransferEntry => ({
    id: entry.id, name: entry.name, isDir: entry.isDir, sizeBytes: entry.sizeBytes,
  });

  const submitMetas = async (metas: Parameters<typeof transfers.submit>[0][]): Promise<void> => {
    for (const meta of metas) await transfers.submit(meta);
  };

  return (
    <main className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-10 shrink-0 flex-wrap items-center gap-2 border-b px-3">
        <Breadcrumb rootPath={browse.rootPath} nodeLabel={LOCAL_NODE.label} onNavigate={browse.setRootPath} />
        <div className="ms-auto flex items-center gap-1">
          <Button variant={view === "list" ? "secondary" : "ghost"} size="sm" aria-pressed={view === "list"} onClick={() => setView("list")}>
            {t("toolbar.viewList")}
          </Button>
          <Button variant={view === "grid" ? "secondary" : "ghost"} size="sm" aria-pressed={view === "grid"} onClick={() => setView("grid")}>
            {t("toolbar.viewGrid")}
          </Button>
          <select
            aria-label={t("browse.sortLabel")}
            value={browse.sort.key}
            onChange={(event) => browse.setSort({ key: event.target.value as SortKey, desc: browse.sort.desc })}
            className="h-7 rounded-md border border-input bg-transparent px-1 text-xs"
          >
            {SORT_KEYS.map((key) => (
              <option key={key} value={key}>{t(SORT_LABEL_KEY[key])}</option>
            ))}
          </select>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={browse.sort.desc ? t("browse.descending") : t("browse.ascending")}
            onClick={() => browse.setSort({ key: browse.sort.key, desc: !browse.sort.desc })}
          >
            <span aria-hidden>{browse.sort.desc ? "↓" : "↑"}</span>
          </Button>
        </div>
      </div>

      {/* T02 操作行:破坏性/跨 Node 操作一律经预览-提交对话框,禁直通。
          单文件删除引擎白名单无对应命令(fs_delete 语义=目录树),显式禁用+说明。 */}
      <div className="flex shrink-0 items-center gap-1 border-b px-3 py-1.5" role="toolbar" aria-label={t("actions.toolbarLabel")}>
        <Button variant="ghost" size="sm" onClick={() => setFolderOpen(true)}>{t("actions.newFolder")}</Button>
        <Button
          variant="ghost"
          size="sm"
          disabled={selected === null}
          onClick={() => setTransferOpen(true)}
        >
          {t("actions.transfer")}
        </Button>
        <Button
          variant="ghost"
          size="sm"
          disabled={selected === null || selected.isDir === false}
          title={selected !== null && selected.isDir === false ? t("actions.deleteFileUnavailable") : undefined}
          onClick={() => setDeleteOpen(true)}
        >
          {t("actions.delete")}
        </Button>
      </div>

      <div className="flex shrink-0 flex-wrap items-center gap-1 border-b px-3 py-1.5">
        <input
          type="text"
          value={chipDraft}
          placeholder={t("browse.filterPlaceholder")}
          aria-label={t("browse.filterPlaceholder")}
          onChange={(event) => setChipDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              browse.addChip(chipDraft);
              setChipDraft("");
            }
          }}
          className="h-7 w-44 rounded-md border border-input bg-transparent px-2 text-xs"
        />
        {browse.chips.map((chip, index) => (
          <span key={chip} className="flex items-center gap-0.5 rounded-full border border-sidebar-border px-2 py-0.5 text-xs">
            {chip}
            <button
              type="button"
              aria-label={`${t("browse.removeChip")}: ${chip}`}
              className="rounded-full px-1 text-muted-foreground hover:bg-muted"
              onClick={() => browse.removeChip(index)}
            >
              ×
            </button>
          </span>
        ))}
      </div>

      <div className="flex min-h-0 flex-1">
        {browse.phase === "skeleton" && (
          <div className="min-h-0 flex-1 p-3" aria-busy="true" aria-label={t("browse.statusLoading")}>
            {Array.from({ length: 8 }, (_, index) => (
              <div key={index} className="mb-2 flex h-8 items-center">
                <span className="inline-block h-3 animate-pulse rounded bg-muted" style={{ width: `${58 - (index % 4) * 9}%` }} />
              </div>
            ))}
          </div>
        )}
        {browse.phase === "empty" && (
          <div className="flex min-h-0 flex-1 items-center justify-center">
            <div className="text-center">
              <p className="text-sm text-muted-foreground">{t("browse.empty")}</p>
              <p className="mt-1 text-xs text-muted-foreground/70">{t("browse.emptyHint")}</p>
            </div>
          </div>
        )}
        {browse.phase === "error" && (
          <div role="alert" className="flex min-h-0 flex-1 flex-col items-center justify-center gap-2">
            <p className="text-sm text-destructive">{t("browse.errorTitle")}</p>
            <p className="max-w-md break-all text-center text-xs text-muted-foreground">{browse.rootError}</p>
            <Button variant="outline" size="sm" onClick={browse.retry}>{t("browse.retry")}</Button>
          </div>
        )}
        {browse.phase === "success" && (
          <>
            {view === "list" ? (
              <BrowseTreeGrid
                data={browse.data}
                expanded={browse.expanded}
                onExpandedChange={browse.onExpandedChange}
                selectedId={selected?.id ?? null}
                onSelect={(item) => {
                  if (item.entry !== null) setSelected(item.entry);
                }}
              />
            ) : (
              <BrowseGrid
                entries={browse.rootEntries}
                selectedId={selected?.id ?? null}
                onSelect={setSelected}
                onOpenDir={browse.setRootPath}
              />
            )}
            <DetailPanel entry={selected} open={detailsOpen} onToggle={() => setDetailsOpen((prev) => !prev)} />
          </>
        )}
      </div>

      {/* T02 对话框:传输=预览-提交(冲突策略);删除=确认(引擎删除语义如实
          展示);新建目录=名称输入。成功后重载当前目录(loadDir 直拉,破缓存)。 */}
      <TransferDialog
        open={transferOpen}
        isMove={false}
        entries={selected !== null ? [toEntry(selected)] : []}
        srcRoot={browse.rootPath}
        node={LOCAL_NODE.label}
        onClose={() => setTransferOpen(false)}
        onSubmit={(metas) => submitMetas(metas)}
      />
      <FolderDialog
        open={folderOpen}
        srcRoot={browse.rootPath}
        node={LOCAL_NODE.label}
        onClose={() => setFolderOpen(false)}
        onCreate={(fs, remote) =>
          commands.fsMkdir(LOCAL_NODE.label, fs, remote, 1).then((result) => {
            if (result.status === "error") throw new Error(`${result.error.kind}: ${result.error.msg}`);
            if (result.data !== "applied") {
              throw new Error(result.data === "exhausted" ? t("transfers.exhausted") : t("transfers.state.throttled"));
            }
            return browse.retry();
          })
        }
      />
      <DeleteConfirm
        open={deleteOpen}
        targetFs={selected !== null ? composeFs(browse.rootPath, selected.name) : browse.rootPath}
        node={LOCAL_NODE.label}
        onClose={() => setDeleteOpen(false)}
        onDelete={(fs) =>
          commands.fsDelete(LOCAL_NODE.label, fs, 1).then((result) => {
            if (result.status === "error") throw new Error(`${result.error.kind}: ${result.error.msg}`);
            if (result.data !== "applied") {
              throw new Error(result.data === "exhausted" ? t("transfers.exhausted") : t("transfers.state.throttled"));
            }
            return browse.retry();
          })
        }
      />
    </main>
  );
}
