// 内容区(静态骨架):仅空态占位,不含任何文件数据(数据面属 WP05/WP07)。
import { t } from "@/i18n";

export function ContentArea() {
  return (
    <main className="flex min-h-0 flex-1 items-center justify-center">
      <div className="text-center">
        <p className="text-sm text-muted-foreground">{t("content.empty")}</p>
        <p className="mt-1 text-xs text-muted-foreground/70">{t("content.emptyHint")}</p>
      </div>
    </main>
  );
}
