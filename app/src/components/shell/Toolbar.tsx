// 工具行:主题三态切换(DoD⑦)+ 应用版本。
// 版本来自示范 IPC 命令 app_version;IPC 失败必须在 UI 上浮(禁止吞错)。
// 面包屑/排序/筛选/视图切换属浏览域,由 BrowseView 提供(T02 起替换占位按钮)。
import { useEffect, useState } from "react";
import { applyTheme, THEME_ORDER, type ThemeName } from "@/design/tokens";
import { t } from "@/i18n";

export type IpcState =
  | { kind: "loading" }
  | { kind: "ok"; version: string }
  | { kind: "error"; message: string };

const THEME_LABEL_KEY: Record<ThemeName, string> = {
  dark: "theme.dark",
  light: "theme.light",
  warm: "theme.warm",
};

export function Toolbar({ ipc }: { ipc: IpcState }) {
  // 主题态:默认暗色(DoD⑦);applyTheme 写 [data-theme] 并同步 .dark 类。
  const [theme, setTheme] = useState<ThemeName>("dark");
  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  return (
    <div className="flex h-10 shrink-0 items-center gap-2 border-b px-3">
      <label className="flex items-center gap-1 text-xs text-muted-foreground">
        <span className="sr-only">{t("theme.label")}</span>
        <select
          aria-label={t("theme.label")}
          value={theme}
          onChange={(event) => {
            const next = event.target.value;
            if (THEME_ORDER.includes(next as ThemeName)) setTheme(next as ThemeName);
          }}
          className="h-7 rounded-md border border-input bg-transparent px-1 text-xs"
        >
          {THEME_ORDER.map((name) => (
            <option key={name} value={name}>{t(THEME_LABEL_KEY[name])}</option>
          ))}
        </select>
      </label>

      <span className="ms-auto w-24 text-right text-xs tabular-nums text-muted-foreground">
        {ipc.kind === "loading" && "…"}
        {ipc.kind === "ok" && `v${ipc.version}`}
        {ipc.kind === "error" && (
          <span className="text-destructive">
            {t("ipc.versionError")}: {ipc.message}
          </span>
        )}
      </span>
    </div>
  );
}
