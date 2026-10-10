// 底部传输条+队列面板(M1-WP06-T02 DoD④,08 §4.4):实数据聚合进度+队列面板
// (方向/源→目标/进度/可解释状态=限速中-该盘限流/排队-预算器/重试中)+失败行内
// 重试/查看原因。文案全英文(AGENTS 语言红线);「待 Owner GUI 实操验收」显式
// 标注(AGENTS 硬规则;无人值守轮=TransferBar.test.tsx 行为断言替代)。
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { t } from "@/i18n";
import { explainStatus, progressPercent, type QueueRow } from "@/lib/transfer/model";
import { errorText, useTransfers } from "@/lib/transfer/useTransfers";

function directionGlyph(kind: string): string {
  return kind === "move" ? "⇄" : "→";
}

function QueueRowLine({ row, expanded, onToggle, onRetry, onCancel, canRetry }: {
  row: QueueRow;
  expanded: boolean;
  onToggle: () => void;
  onRetry: () => void;
  onCancel: () => void;
  canRetry: boolean;
}) {
  const { record, pending } = row;
  const statusKey = pending !== null ? "transfers.state.throttled" : record !== null ? explainStatus(record).key : "transfers.state.queued";
  const detail = pending === null && record !== null ? explainStatus(record).detail : null;
  const kind = pending !== null ? pending.meta.kind : (record?.kind ?? "copy");
  const src = pending !== null ? pending.meta.src : (record?.src ?? "");
  const dst = pending !== null ? pending.meta.dst : (record?.dst ?? "");
  const percent = record !== null ? progressPercent(record) : null;
  const cancellable = record !== null && (record.status === "queued" || record.status === "running");
  const failed = record !== null && record.status === "error" && record.error !== "user_canceled";
  return (
    <li className="flex flex-col gap-1 border-b px-3 py-1.5 text-xs last:border-b-0">
      <div className="flex min-w-0 items-center gap-2">
        <span aria-hidden className="w-4 shrink-0 text-center font-medium">{directionGlyph(kind)}</span>
        <span className="min-w-0 flex-1 truncate" title={`${src} ${directionGlyph(kind)} ${dst}`}>
          {src} {directionGlyph(kind)} {dst}
        </span>
        <span className="w-12 shrink-0 text-end tabular-nums text-muted-foreground">
          {percent === null ? "—" : `${percent}%`}
        </span>
        <span className="w-36 shrink-0 text-end text-muted-foreground">{t(statusKey)}</span>
        {failed && (
          <>
            <Button variant="ghost" size="sm" onClick={onToggle} aria-expanded={expanded}>
              {t("transfers.details")}
            </Button>
            <Button variant="outline" size="sm" disabled={!canRetry} title={canRetry ? undefined : t("transfers.metaMissing")} onClick={onRetry}>
              {t("transfers.retry")}
            </Button>
          </>
        )}
        {cancellable && (
          <Button variant="ghost" size="sm" onClick={onCancel}>{t("transfers.cancel")}</Button>
        )}
      </div>
      {expanded && detail !== null && (
        <p className="break-all rounded bg-muted px-2 py-1 text-muted-foreground" role="note">
          {detail}
        </p>
      )}
      {pending !== null && (
        <p className="text-muted-foreground" role="note">{t("transfers.throttledHint")}</p>
      )}
    </li>
  );
}

export function TransferBar() {
  const store = useTransfers();
  const [open, setOpen] = useState(false);
  const [expandedKey, setExpandedKey] = useState<string | null>(null);
  const { aggregate, error } = store;
  const percent =
    aggregate.totalBytes === null || aggregate.totalBytes === 0
      ? null
      : Math.min(100, Math.floor((aggregate.bytes * 100) / aggregate.totalBytes));
  const summary =
    aggregate.activeCount === 0
      ? t("transfers.idle")
      : percent === null
        ? t("transfers.activeIndeterminate").replace("{count}", String(aggregate.activeCount))
        : t("transfers.activePercent").replace("{count}", String(aggregate.activeCount)).replace("{percent}", String(percent));
  return (
    <footer className="flex shrink-0 flex-col border-t text-xs">
      <div className="flex h-9 items-center gap-3 px-3 text-muted-foreground">
        <button
          type="button"
          className="font-medium text-foreground hover:underline"
          aria-expanded={open}
          aria-controls="transfer-queue"
          onClick={() => setOpen((prev) => !prev)}
        >
          {t("transfers.title")}
        </button>
        <div
          className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted"
          role="progressbar"
          aria-label={t("transfers.progressLabel")}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent ?? undefined}
        >
          <div className="h-full rounded-full bg-brand-500" style={{ width: `${percent ?? 0}%` }} />
        </div>
        <span>{summary}</span>
        {error !== null && (
          <span className="truncate text-destructive" role="alert" title={error}>{error}</span>
        )}
      </div>
      {open && (
        <ul id="transfer-queue" role="region" aria-label={t("transfers.queueLabel")} className="max-h-56 overflow-y-auto border-t">
          {store.rows.length === 0 && (
            <li className="px-3 py-2 text-muted-foreground">{t("transfers.empty")}</li>
          )}
          {store.rows.map((row) => (
            <QueueRowLine
              key={row.key}
              row={row}
              expanded={expandedKey === row.key}
              onToggle={() => setExpandedKey((prev) => (prev === row.key ? null : row.key))}
              onRetry={() => {
                if (row.record !== null)
                  // 审查 [medium] 修正:rejection 写入错误面(禁吞错),不再 void 丢弃。
                  store.retry(row.record.id).catch((err: unknown) => store.reportError(errorText(err)));
              }}
              onCancel={() => {
                if (row.record !== null)
                  store.cancel(row.record.id).catch((err: unknown) => store.reportError(errorText(err)));
              }}
              canRetry={row.record !== null && store.hasMeta(row.record.id)}
            />
          ))}
        </ul>
      )}
    </footer>
  );
}
