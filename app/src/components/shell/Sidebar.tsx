// 侧栏(静态骨架,无数据):分区锚点结构自 V1 一次到位(08 规划 §10.1)。
// 分区 = Nodes(本地/网盘/协议分组头)+ 预留组「设备与团队」+「场景库」锚点。
// 文案全部走 i18n 资源(仅英文);无任何业务行为。
import { Button } from "@/components/ui/button";
import { t } from "@/i18n";

function GroupHeader({ label }: { label: string }) {
  return (
    <div className="px-2 pt-3 pb-1 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
      {label}
    </div>
  );
}

function SceneEntry({ label }: { label: string }) {
  // 静态骨架:入口仅呈现,不挂任何处理逻辑(导航属 WP05)。
  return (
    <button
      type="button"
      className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-sm text-sidebar-foreground/90 hover:bg-sidebar-accent"
    >
      <span aria-hidden className="size-1.5 rounded-full bg-brand-500" />
      {label}
    </button>
  );
}

export function Sidebar() {
  return (
    <aside className="flex w-60 shrink-0 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground">
      <div className="flex items-center gap-2 px-4 py-3">
        <span aria-hidden className="size-3 rounded-full bg-brand-500" />
        <span className="text-sm font-semibold">{t("app.title")}</span>
      </div>

      <nav className="flex-1 overflow-y-auto px-2 pb-2">
        <GroupHeader label={t("sidebar.nodesSection")} />
        <GroupHeader label={t("sidebar.groupLocal")} />
        <GroupHeader label={t("sidebar.groupCloud")} />
        <GroupHeader label={t("sidebar.groupProtocols")} />
        <div className="px-2 py-1 text-xs text-muted-foreground">{t("sidebar.emptyHint")}</div>

        {/* 预留组(V5 多设备):仅分组锚点,占位不实现 */}
        <GroupHeader label={t("sidebar.reservedDevices")} />
        <div className="px-2">
          <span className="inline-block rounded border border-sidebar-border px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">
            {t("sidebar.reservedTag")}
          </span>
        </div>

        {/* 场景库锚点:V1 仅文件/照片馆/文档库入口(08 规划 §10.1) */}
        <GroupHeader label={t("sidebar.sceneLibrary")} />
        <SceneEntry label={t("sidebar.sceneFiles")} />
        <SceneEntry label={t("sidebar.scenePhotos")} />
        <SceneEntry label={t("sidebar.sceneDocs")} />
      </nav>

      <div className="border-t border-sidebar-border p-3">
        <Button variant="outline" size="sm" className="w-full" disabled>
          {t("sidebar.addNode")}
        </Button>
      </div>
    </aside>
  );
}
