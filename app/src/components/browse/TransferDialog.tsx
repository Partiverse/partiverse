// 文件操作对话框(M1-WP06-T02 DoD⑤,08 §4.4 预览-提交):跨 Node 复制/移动
// 预览-提交(冲突四策略:跳过/覆盖/重命名/两者保留;重命名类依赖服务端单文件
// 能力协商,caps 采集属 WP09——无 caps 时显式禁用并说明,禁静默降级)+ 新建
// 目录 + 删除确认(破坏性操作禁直通,如实展示引擎删除语义)。文案全英文。
import { Dialog } from "radix-ui";
import { useEffect, useMemo, useState } from "react";
import { commands } from "@/bindings";
import { Button } from "@/components/ui/button";
import { t } from "@/i18n";
import { parseDirListing } from "@/lib/browse/model";
import {
  composeFs, dirTransferParams, parentRemote, planTransfer, singleFileParams,
  type ConflictPolicy, type TransferEntry,
} from "@/lib/transfer/model";
import type { SubmitMeta } from "@/lib/transfer/model";

const FIELD =
  "h-8 w-full rounded-md border border-input bg-transparent px-2 text-xs";

/** 重命名类策略落盘依赖的服务端能力:今日 caps 采集属 WP09(fsinfo/backend
 * features),无协商来源 → 显式 false;WP09 落地后此开关点亮,零 UI 结构改动。 */
const SERVER_SIDE_AVAILABLE = false;

const POLICIES: readonly { value: ConflictPolicy; labelKey: string }[] = [
  { value: "skip", labelKey: "dialog.policySkip" },
  { value: "overwrite", labelKey: "dialog.policyOverwrite" },
  { value: "rename", labelKey: "dialog.policyRename" },
  { value: "keep-both", labelKey: "dialog.policyKeepBoth" },
];

export interface TransferDialogProps {
  open: boolean;
  isMove: boolean;
  entries: TransferEntry[];
  /** 当前目录 fs(源父;本地裸路径或 remote:path)。 */
  srcRoot: string;
  /** 预算计量 Node 键(无 profile = 不限流)。 */
  node: string;
  onClose: () => void;
  /** 提交回调(宿主经传输 store 串行 submit,重试元数据随 store 存活)。 */
  onSubmit: (metas: SubmitMeta[]) => Promise<void>;
}

export function TransferDialog({ open, isMove, entries, srcRoot, node, onClose, onSubmit }: TransferDialogProps) {
  const [dst, setDst] = useState("");
  const [dstNames, setDstNames] = useState<Set<string> | null>(null);
  const [dstError, setDstError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [policy, setPolicy] = useState<ConflictPolicy>("overwrite");
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  // 目标目录探测(300ms 防抖):operationsList → 同名冲突集合;失败显式上浮,
  // 冲突面缺失时禁提交(禁静默直通)。全部状态更新在宏任务内(react-hooks
  // set-state-in-effect:禁 effect 体内同步级联 setState),未打开/空目标走
  // 同一定时器复位。
  useEffect(() => {
    let cancelled = false;
    const timer = window.setTimeout(() => {
      if (!open || dst.trim() === "") {
        setDstNames(null);
        setDstError(null);
        setChecking(false);
        return;
      }
      setChecking(true);
      commands
        // operations/list 两键齐传(remote 空串合法,T09 实测形状:仅 fs = 400)。
        .operationsList(dst.trim(), "")
        .then((result) => {
          if (cancelled) return;
          if (result.status === "error") throw new Error(`${result.error.kind}: ${result.error.msg}`);
          const parsed = parseDirListing(result.data, dst.trim(), "");
          if (!parsed.ok) throw new Error(parsed.error);
          setDstNames(new Set(parsed.entries.map((entry) => entry.name)));
          setDstError(null);
        })
        .catch((err: unknown) => {
          if (cancelled) return;
          setDstNames(null);
          setDstError(err instanceof Error ? err.message : JSON.stringify(err));
        })
        .finally(() => {
          if (!cancelled) setChecking(false);
        });
    }, 300);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [open, dst]);

  const plan = useMemo(
    () => planTransfer(entries, dstNames ?? new Set(), policy, SERVER_SIDE_AVAILABLE),
    [entries, dstNames, policy],
  );
  const submitCount = plan.items.filter((item) => item.action === "submit").length;
  const renamePolicy = policy === "rename" || policy === "keep-both";
  const blocked = renamePolicy && plan.renamesBlocked > 0;
  const canSubmit = !checking && dstNames !== null && submitCount > 0 && !blocked && !submitting;

  const handleSubmit = () => {
    const metas = plan.items
      .filter((item) => item.action === "submit")
      .map((item) => {
        const name = item.entry.name;
        if (item.entry.isDir) {
          // 目录:srcFs=源子目录内容 → dstFs=目标同名子目录(rclone 合并语义)。
          const srcFs = composeFs(srcRoot, name);
          const dstFs = composeFs(dst.trim(), name);
          return {
            node, method: isMove ? "sync/move" : "sync/copy", kind: isMove ? "move" : "copy",
            src: srcFs, dst: dstFs, params: dirTransferParams(srcFs, dstFs), cost: 1,
          };
        }
        // 单文件:srcFs=源父目录 + FilterRule 精确选中(core fsops 实测锚定;
        // srcFs 指向文件本身必败)。
        const parent = parentRemote(name) ?? "";
        return {
          node, method: isMove ? "sync/move" : "sync/copy", kind: isMove ? "move" : "copy",
          src: composeFs(srcRoot, name), dst: dst.trim(),
          params: singleFileParams(composeFs(srcRoot, parent), dst.trim(), name), cost: 1,
        };
      });
    setSubmitting(true);
    setSubmitError(null);
    onSubmit(metas)
      .then(onClose)
      .catch((err: unknown) => {
        setSubmitError(err instanceof Error ? err.message : JSON.stringify(err));
      })
      .finally(() => setSubmitting(false));
  };

  return (
    <Dialog.Root open={open} onOpenChange={(next) => { if (!next) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/50" />
        <Dialog.Content className="fixed left-1/2 top-1/2 flex max-h-[80vh] w-[480px] -translate-x-1/2 -translate-y-1/2 flex-col gap-2 overflow-y-auto rounded-lg border bg-background p-4 shadow-lg">
          <Dialog.Title className="text-sm font-semibold">
            {isMove ? t("dialog.moveTitle") : t("dialog.copyTitle")}
          </Dialog.Title>
          <Dialog.Description className="text-xs text-muted-foreground">{t("dialog.previewHint")}</Dialog.Description>
          <label className="flex flex-col gap-1 text-xs">
            {t("dialog.destination")}
            <input type="text" className={FIELD} value={dst} onChange={(event) => setDst(event.target.value)} />
          </label>
          {checking && <p className="text-xs text-muted-foreground">{t("dialog.checking")}</p>}
          {dstError !== null && (
            <p role="alert" className="text-xs text-destructive">{t("dialog.destError")}: {dstError}</p>
          )}
          <fieldset className="flex flex-col gap-1 text-xs">
            <legend className="mb-1 text-muted-foreground">{t("dialog.entries")}</legend>
            <ul className="max-h-28 overflow-y-auto rounded border px-2 py-1">
              {entries.map((entry) => {
                const conflict = dstNames?.has(entry.name) === true && !entry.isDir;
                const item = plan.items.find((candidate) => candidate.entry.id === entry.id);
                return (
                  <li key={entry.id} className="flex items-center gap-2 py-0.5">
                    <span className="min-w-0 flex-1 truncate">{entry.name}</span>
                    {conflict && <span className="text-amber-600">{t("dialog.conflictBadge")}</span>}
                    {item !== undefined && item.action === "submit" && item.dstName !== entry.name && (
                      <span className="text-muted-foreground">→ {item.dstName}</span>
                    )}
                    {item !== undefined && item.action === "skip" && (
                      <span className="text-muted-foreground">{t("dialog.policySkip")}</span>
                    )}
                  </li>
                );
              })}
            </ul>
          </fieldset>
          <fieldset className="flex flex-col gap-1 text-xs">
            <legend className="mb-1 text-muted-foreground">{t("dialog.policy")}</legend>
            <div className="flex flex-wrap gap-3">
              {POLICIES.map(({ value, labelKey }) => {
                const renameOption = value === "rename" || value === "keep-both";
                const disabled = renameOption && !SERVER_SIDE_AVAILABLE;
                return (
                  <label key={value} className="flex items-center gap-1" title={disabled ? t("dialog.renameUnavailable") : undefined}>
                    <input
                      type="radio"
                      name="conflict-policy"
                      value={value}
                      disabled={disabled}
                      checked={policy === value}
                      onChange={() => setPolicy(value)}
                    />
                    {t(labelKey)}
                  </label>
                );
              })}
            </div>
            {!SERVER_SIDE_AVAILABLE && (
              <p className="text-muted-foreground" role="note">{t("dialog.renameUnavailable")}</p>
            )}
            {blocked && (
              <p className="text-amber-600" role="alert">
                {t("dialog.renameBlocked").replace("{count}", String(plan.renamesBlocked))}
              </p>
            )}
          </fieldset>
          {submitError !== null && (
            <p role="alert" className="break-all text-xs text-destructive">{submitError}</p>
          )}
          <div className="mt-1 flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={onClose} disabled={submitting}>{t("dialog.cancel")}</Button>
            <Button variant="default" size="sm" disabled={!canSubmit} onClick={handleSubmit}>
              {submitting ? t("dialog.submitting") : t("dialog.submit").replace("{count}", String(submitCount))}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export interface FolderDialogProps {
  open: boolean;
  /** 当前目录 fs(新建目录的父)。 */
  srcRoot: string;
  node: string;
  onClose: () => void;
  /** 宿主回调:commands.fsMkdir 包装(重试元数据无需持久,同步操作)。 */
  onCreate: (fs: string, remote: string) => Promise<void>;
}

export function FolderDialog({ open, srcRoot, node, onClose, onCreate }: FolderDialogProps) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const handleCreate = () => {
    const trimmed = name.trim();
    if (trimmed === "" || busy) return;
    setBusy(true);
    setError(null);
    onCreate(srcRoot, trimmed)
      .then(() => {
        setName("");
        onClose();
      })
      .catch((err: unknown) => setError(err instanceof Error ? err.message : JSON.stringify(err)))
      .finally(() => setBusy(false));
  };
  return (
    <Dialog.Root open={open} onOpenChange={(next) => { if (!next) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/50" />
        <Dialog.Content className="fixed left-1/2 top-1/2 w-[380px] -translate-x-1/2 -translate-y-1/2 rounded-lg border bg-background p-4 shadow-lg">
          <Dialog.Title className="text-sm font-semibold">{t("folder.title")}</Dialog.Title>
          <Dialog.Description className="mt-0.5 break-all text-xs text-muted-foreground">{node} · {srcRoot}</Dialog.Description>
          <label className="mt-2 flex flex-col gap-1 text-xs">
            {t("folder.name")}
            <input
              type="text"
              className={FIELD}
              value={name}
              autoFocus
              onChange={(event) => setName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") handleCreate();
              }}
            />
          </label>
          {error !== null && <p role="alert" className="mt-2 break-all text-xs text-destructive">{error}</p>}
          <div className="mt-3 flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={onClose} disabled={busy}>{t("folder.cancel")}</Button>
            <Button variant="default" size="sm" disabled={name.trim() === "" || busy} onClick={handleCreate}>
              {busy ? t("folder.creating") : t("folder.create")}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export interface DeleteConfirmProps {
  open: boolean;
  /** 目标目录全路径(fs 串;引擎语义 = 递归删其下全部文件、保留目录壳)。 */
  targetFs: string;
  node: string;
  onClose: () => void;
  onDelete: (fs: string) => Promise<void>;
}

export function DeleteConfirm({ open, targetFs, node, onClose, onDelete }: DeleteConfirmProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const handleDelete = () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    onDelete(targetFs)
      .then(onClose)
      .catch((err: unknown) => setError(err instanceof Error ? err.message : JSON.stringify(err)))
      .finally(() => setBusy(false));
  };
  return (
    <Dialog.Root open={open} onOpenChange={(next) => { if (!next) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/50" />
        <Dialog.Content className="fixed left-1/2 top-1/2 w-[420px] -translate-x-1/2 -translate-y-1/2 rounded-lg border bg-background p-4 shadow-lg">
          <Dialog.Title className="text-sm font-semibold">{t("confirmDelete.title")}</Dialog.Title>
          <Dialog.Description className="mt-1 break-all text-xs text-muted-foreground">
            {t("confirmDelete.body").replace("{path}", targetFs)}
          </Dialog.Description>
          <p className="mt-2 text-xs text-amber-600">{node} · {t("confirmDelete.engineSemantics")}</p>
          {error !== null && <p role="alert" className="mt-2 break-all text-xs text-destructive">{error}</p>}
          <div className="mt-3 flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={onClose} disabled={busy}>{t("confirmDelete.cancel")}</Button>
            <Button variant="destructive" size="sm" disabled={busy} onClick={handleDelete}>
              {busy ? t("confirmDelete.deleting") : t("confirmDelete.confirm")}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
