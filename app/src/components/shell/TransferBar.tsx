// 底部传输条(静态占位):聚合进度条为空轨道,无任务数据(传输编排属 WP03/WP06)。
import { t } from "@/i18n";

export function TransferBar() {
  return (
    <footer className="flex h-9 shrink-0 items-center gap-3 border-t px-3 text-xs text-muted-foreground">
      <span className="font-medium">{t("transfers.title")}</span>
      <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
        <div className="h-full w-0 rounded-full bg-brand-500" />
      </div>
      <span>{t("transfers.idle")}</span>
    </footer>
  );
}
