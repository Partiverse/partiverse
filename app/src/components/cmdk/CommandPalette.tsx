// ⌘K 命令面板(DoD④):cmdk 三区(搜文件=当前目录/搜 Node/搜命令),输入防抖+
// 本地索引同步检索(响应 <100ms,基准断言见 lib/cmdk 单测);文案全走 i18n(仅英文)。
import { Command } from "cmdk";
import { useEffect, useMemo, useState } from "react";

import { t } from "@/i18n";
import { buildIndex, PALETTE_DEBOUNCE_MS, search, ZONE_ORDER, type PaletteEntry } from "@/lib/cmdk";
import { usePaletteSource } from "@/lib/cmdk/sourceStore";

const ZONE_LABEL_KEY: Record<PaletteEntry["zone"], string> = { files: "palette.zoneFiles", nodes: "palette.zoneNodes", commands: "palette.zoneCommands" };

const COMMAND_ACTIONS: Array<{ id: string; labelKey: string; action: "onAddNode" | "onOpenDiagnostics" | "onBrowse" }> = [
  { id: "cmd.add-node", labelKey: "palette.cmdAddNode", action: "onAddNode" },
  { id: "cmd.diagnostics", labelKey: "palette.cmdDiagnostics", action: "onOpenDiagnostics" },
  { id: "cmd.browse", labelKey: "palette.cmdBrowse", action: "onBrowse" },
];

/** 输入防抖(值视角):窗口内连续输入只取末值;40ms 窗口保 <100ms 总响应。 */
function useDebouncedValue(value: string, waitMs: number): string {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), waitMs);
    return () => clearTimeout(timer);
  }, [value, waitMs]);
  return debounced;
}

export function CommandPalette({ open, onOpenChange, actions }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  actions: { onAddNode: () => void; onOpenDiagnostics: () => void; onBrowse: () => void };
}) {
  const source = usePaletteSource();
  const [query, setQuery] = useState("");
  const debouncedQuery = useDebouncedValue(query, PALETTE_DEBOUNCE_MS);

  // Node 区:V1 浏览根=本地盘(真实节点,useBrowse 发布);远端 Node 枚举命令未交付
  // (T02 同口径),空态诚实呈现,不伪造条目。
  const index = useMemo(
    () => buildIndex([
      ...source.files,
      ...(source.nodeLabel === "" ? [] : [{ id: `node:${source.nodeLabel}`, zone: "nodes" as const, label: source.nodeLabel, detail: source.path }]),
      ...COMMAND_ACTIONS.map((cmd) => ({ id: cmd.id, zone: "commands" as const, label: t(cmd.labelKey), detail: null })),
    ]),
    [source.files, source.nodeLabel, source.path],
  );
  const results = useMemo(() => search(index, debouncedQuery), [index, debouncedQuery]);

  const run = (entry: PaletteEntry): void => {
    onOpenChange(false);
    const action = COMMAND_ACTIONS.find((cmd) => cmd.id === entry.id);
    if (action !== undefined) actions[action.action]();
    else if (entry.zone === "nodes") actions.onBrowse();
  };

  return (
    <Command.Dialog
      open={open}
      onOpenChange={onOpenChange}
      label={t("palette.title")}
      shouldFilter={false}
      overlayClassName="fixed inset-0 bg-black/50"
      contentClassName="fixed left-1/2 top-24 w-[560px] -translate-x-1/2 rounded-lg border bg-background p-2 shadow-lg"
    >
      <Command.Input value={query} onValueChange={setQuery} placeholder={t("palette.placeholder")} className="w-full border-b bg-transparent px-2 py-2 text-sm outline-none" />
      <Command.List className="max-h-80 overflow-auto py-1">
        <Command.Empty className="px-2 py-3 text-center text-xs text-muted-foreground">{t("palette.empty")}</Command.Empty>
        {ZONE_ORDER.map((zone) => (
          <Command.Group key={zone} heading={t(ZONE_LABEL_KEY[zone])} className="px-1 py-1 text-[11px] text-muted-foreground [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1">
            {zone === "nodes" && index.entries.every((entry) => entry.zone !== "nodes") && (
              <div className="px-2 py-1 text-xs text-muted-foreground">{t("palette.noNodes")}</div>
            )}
            {results.filter((entry) => entry.zone === zone).map((entry) => (
              <Command.Item
                key={entry.id}
                value={entry.id}
                onSelect={() => run(entry)}
                className="flex cursor-pointer items-center gap-2 rounded px-2 py-1.5 text-sm data-[selected=true]:bg-accent"
              >
                <span className="truncate">{entry.label}</span>
                {entry.detail !== null && <span className="ms-auto truncate text-xs text-muted-foreground">{entry.detail}</span>}
              </Command.Item>
            ))}
          </Command.Group>
        ))}
      </Command.List>
    </Command.Dialog>
  );
}
