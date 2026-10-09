// 工具行(静态骨架):面包屑占位 + 视图切换/排序占位 + 搜索占位 + 应用版本。
// 版本来自示范 IPC 命令 app_version;IPC 失败必须在 UI 上浮(禁止吞错)。
import { Button } from "@/components/ui/button";
import { t } from "@/i18n";

export type IpcState =
  | { kind: "loading" }
  | { kind: "ok"; version: string }
  | { kind: "error"; message: string };

export function Toolbar({ ipc }: { ipc: IpcState }) {
  return (
    <div className="flex h-10 shrink-0 items-center gap-2 border-b px-3">
      <span className="text-sm text-muted-foreground">{t("toolbar.breadcrumbRoot")}</span>

      <div className="ml-auto flex items-center gap-1">
        <Button variant="secondary" size="sm">
          {t("toolbar.viewList")}
        </Button>
        <Button variant="ghost" size="sm">
          {t("toolbar.viewGrid")}
        </Button>
        <Button variant="ghost" size="sm">
          {t("toolbar.sort")}
        </Button>
        <input
          type="text"
          placeholder={t("toolbar.searchPlaceholder")}
          disabled
          className="h-7 w-40 rounded-md border border-input bg-transparent px-2 text-xs text-muted-foreground placeholder:text-muted-foreground/60 disabled:opacity-60"
        />
        <span className="ml-2 w-24 text-right text-xs tabular-nums text-muted-foreground">
          {ipc.kind === "loading" && "…"}
          {ipc.kind === "ok" && `v${ipc.version}`}
          {ipc.kind === "error" && (
            <span className="text-destructive">
              {t("ipc.versionError")}: {ipc.message}
            </span>
          )}
        </span>
      </div>
    </div>
  );
}
