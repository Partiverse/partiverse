// 添加 Node 向导(DoD①②③):四分支 ≤3 步(Provider→Configure→Review & Connect);RHF+zod
// 动态表单消费 WP04 providers_fetch 描述模型(协议类高级项折叠);提交前一律预览-提交对话框
// (secret 脱敏走 lib/wizard/model,禁入日志/localStorage);完成即轻探针体检(预算 acquire 前置
// +operations/list 单次)。仅 123 分支有已交付 IPC 创建通道,其余分支显式上浮「通道未交付」
// 错误态(禁静默/禁伪造成功);文案全走 i18n(仅英文)。
import { Dialog } from "radix-ui";
import { useCallback, useEffect, useMemo, useState, type FormEvent } from "react";
import { useForm, type UseFormReturn } from "react-hook-form";
import { commands, type CmdError, type FieldDesc, type ProviderFormOut } from "@/bindings";
import { parseDirListing } from "@/lib/browse/model";
import { t } from "@/i18n";
import {
  aggregateProbe, branchGroupKey, branchGuidanceKeys, buildDefaultValues, buildZodSchema, createChannel,
  PAN123_FIELDS, BAIDU_FIELDS, redactValues, routeBranch, type ProbeOutcome, type WizardBranch,
} from "@/lib/wizard/model";
import { Button } from "@/components/ui/button";

type ProvidersState = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; providers: ProviderFormOut[] };
type WizardValues = Record<string, string | boolean>;
type SubmitState = { kind: "idle" } | { kind: "busy"; stage: "creating" | "probing" } | { kind: "channel"; message: string }
  | { kind: "error"; message: string } | { kind: "probe"; outcome: ProbeOutcome };
type Selection = { branch: WizardBranch; provider: ProviderFormOut };
type FetchOutcome = { kind: "ready"; providers: ProviderFormOut[] } | { kind: "error"; message: string };

const BRANCH_ORDER: WizardBranch[] = ["pan123", "baidu", "oauth", "protocol"];
const STEPS = ["wizard.stepProvider", "wizard.stepConfigure", "wizard.stepReview"];
/** 常用字段标签映射;未映射字段回退=原生字段名(rclone 协议面英文词汇)。 */
const FIELD_LABEL_KEYS: Record<string, string> = {
  name: "wizard.field.name", endpoint: "wizard.field.endpoint", account: "wizard.field.account", app_password: "wizard.field.appPassword",
  client_id: "wizard.field.clientId", client_secret: "wizard.field.clientSecret", code: "wizard.field.code", user: "wizard.field.user",
  pass: "wizard.field.pass", host: "wizard.field.host", port: "wizard.field.port",
};

const envelopeError = (error: CmdError): string => `${error.kind}: ${error.msg}`;
const errorText = (err: unknown): string => (err instanceof Error ? err.message : JSON.stringify(err));
const asString = (value: string | boolean | undefined): string => (typeof value === "string" ? value : "");
const fieldLabel = (name: string): string => (FIELD_LABEL_KEYS[name] !== undefined ? t(FIELD_LABEL_KEYS[name]) : name);
const fieldsFor = (selection: Selection): FieldDesc[] =>
  selection.branch === "pan123" ? PAN123_FIELDS : selection.branch === "baidu" ? BAIDU_FIELDS : selection.provider.fields;

/** provider schema 拉取(模块级:不捕获 setState,零同步副作用的可复用结果函数)。 */
async function fetchProviderForms(): Promise<FetchOutcome> {
  try {
    const result = await commands.providersFetch();
    return result.status === "error" ? { kind: "error", message: envelopeError(result.error) } : { kind: "ready", providers: result.data };
  } catch (err) {
    return { kind: "error", message: errorText(err) };
  }
}

function FieldRow({ field, form, placeholderKey }: { field: FieldDesc; form: UseFormReturn<WizardValues>; placeholderKey: string | null }) {
  const error = form.formState.errors[field.name]?.message;
  return (
    <label className="flex flex-col gap-1 text-xs">
      <span className="text-muted-foreground">{fieldLabel(field.name)}{field.is_password ? ` · ${t("wizard.secret")}` : ""}</span>
      {field.field_type === "bool"
        ? <input type="checkbox" aria-label={fieldLabel(field.name)} {...form.register(field.name)} className="size-4" />
        : <input type={field.is_password ? "password" : "text"} autoComplete="off" aria-label={fieldLabel(field.name)}
          placeholder={placeholderKey !== null ? t(placeholderKey) : undefined} {...form.register(field.name)}
          className="h-8 rounded-md border border-input bg-transparent px-2 text-sm" />}
      {error !== undefined && <span role="alert" className="text-destructive">{error}</span>}
    </label>
  );
}

/** 动态表单体(DoD①):协议类高级项折叠 <details>;分支引导面板按 model 有序 key 渲染。 */
function ConfigureForm({ fields, branch, providerName, form, onBack, onNext }: {
  fields: FieldDesc[]; branch: WizardBranch; providerName: string; form: UseFormReturn<WizardValues>; onBack: () => void; onNext: () => void;
}) {
  const schema = useMemo(() => buildZodSchema(fields), [fields]);
  const advanced = fields.filter((field) => field.advanced);
  const renderFields = (list: FieldDesc[]) => list.map((field) => (
    <FieldRow key={field.name} field={field} form={form} placeholderKey={branch === "pan123" && field.name === "endpoint" ? "wizard.123.endpointPlaceholder" : null} />
  ));

  const onSubmit = (event: FormEvent<HTMLFormElement>): void => {
    void form.handleSubmit((values) => {
      const parsed = schema.safeParse(values);
      if (!parsed.success) {
        for (const issue of parsed.error.issues) {
          const key = issue.path[0];
          // 审查 [low] 修正:required(input 缺失,zod v4 实测形状)与类型失败分开文案。
          const message =
            issue.input === undefined ? t("wizard.fieldRequired") : t("wizard.fieldInvalid");
          if (typeof key === "string") form.setError(key, { message });
          else form.setError("root", { message: t("wizard.formInvalid") });
        }
        return;
      }
      onNext();
    })(event);
  };

  return (
    <form className="flex min-h-0 flex-col gap-3" onSubmit={onSubmit}>
      {branchGuidanceKeys(branch, providerName).map((key) => (
        <p key={key} className="rounded-md border border-sidebar-border bg-muted/40 px-2 py-1.5 text-xs text-muted-foreground">{t(key)}</p>
      ))}
      {renderFields(fields.filter((field) => !field.advanced))}
      {advanced.length > 0 && (
        <details className="rounded-md border border-sidebar-border px-2 py-1.5">
          <summary className="cursor-pointer text-xs text-muted-foreground">{t("wizard.advanced")}</summary>
          <div className="mt-2 flex flex-col gap-3">{renderFields(advanced)}</div>
        </details>
      )}
      <div className="mt-1 flex justify-end gap-2">
        <Button type="button" variant="outline" size="sm" onClick={onBack}>{t("wizard.back")}</Button>
        <Button type="submit" size="sm">{t("wizard.next")}</Button>
      </div>
    </form>
  );
}

/** 连接体检面板(DoD③):能力位 + 可解释等待 + 失败诊断步骤。 */
function ProbePanel({ outcome, onRetry, onDone }: { outcome: ProbeOutcome; onRetry: () => void; onDone: () => void }) {
  return (
    <div className="flex min-h-0 flex-col gap-2">
      <p className="text-sm font-medium">{t("wizard.probe.title")}</p>
      {outcome.status === "ok" && <p className="text-sm">{t("wizard.probe.ok")}</p>}
      {outcome.status === "throttled" && <p className="text-sm">{t("wizard.probe.throttled")} {outcome.waitUntilMs !== null ? new Date(outcome.waitUntilMs).toISOString() : ""}</p>}
      {outcome.status === "error" && (
        <>
          <p role="alert" className="text-sm text-destructive">{t("wizard.probe.error")}</p>
          {outcome.error !== null && <p className="break-all text-xs text-muted-foreground">{outcome.error}</p>}
        </>
      )}
      {outcome.capabilityBits.length > 0 && (
        <ul className="flex flex-col gap-1">
          {outcome.capabilityBits.map((bit) => (
            <li key={bit.key} className="flex items-center gap-2 text-xs">
              <span aria-hidden className={bit.state === "ok" ? "text-brand-500" : "text-muted-foreground"}>{bit.state === "ok" ? "●" : "○"}</span>
              {t(bit.key)}
              {bit.state === "reserved" && <span className="rounded border border-sidebar-border px-1 text-[10px] uppercase text-muted-foreground">{t("wizard.probe.reserved")}</span>}
            </li>
          ))}
        </ul>
      )}
      {outcome.diagnosticsKeys.length > 0 && (
        <div className="rounded-md border border-sidebar-border bg-muted/40 p-2">
          <p className="text-xs font-medium">{t("wizard.probe.diagTitle")}</p>
          <ul className="mt-1 flex flex-col gap-1">
            {outcome.diagnosticsKeys.map((key) => <li key={key} className="text-xs text-muted-foreground">{t(key)}</li>)}
          </ul>
        </div>
      )}
      <div className="flex justify-end gap-2">
        {outcome.status !== "ok" && <Button variant="outline" size="sm" onClick={onRetry}>{t("wizard.probe.retry")}</Button>}
        <Button size="sm" onClick={onDone}>{t("wizard.done")}</Button>
      </div>
    </div>
  );
}

export function AddNodeWizard({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [providers, setProviders] = useState<ProvidersState>({ kind: "loading" });
  const [selected, setSelected] = useState<Selection | null>(null);
  const [step, setStep] = useState<1 | 2 | 3>(1);
  const [previewOpen, setPreviewOpen] = useState(false);
  const [submit, setSubmit] = useState<SubmitState>({ kind: "idle" });
  const form = useForm<WizardValues>();

  // 打开即拉取 schema;await 之后才写状态(App.tsx 同款 .then 模式,禁 effect 内同步 setState)。
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    void fetchProviderForms().then((outcome) => {
      if (!cancelled) setProviders(outcome);
    });
    return () => {
      cancelled = true;
    };
  }, [open]);

  // 关闭即复位(打开态恒=初始态);状态迁移点收口,不在 effect 内同步 setState。
  const close = useCallback((): void => {
    setStep(1);
    setSelected(null);
    setSubmit({ kind: "idle" });
    setPreviewOpen(false);
    onClose();
  }, [onClose]);

  const selectProvider = (provider: ProviderFormOut): void => {
    const selection: Selection = { branch: routeBranch(provider.name), provider };
    setSelected(selection);
    form.reset(buildDefaultValues(fieldsFor(selection)));
    setSubmit({ kind: "idle" });
    setStep(2);
  };

  // 轻探针(DoD③):预算 acquire 前置(throttled/exhausted 零引擎触达),放行后 operations/list 单次。
  const runProbe = useCallback(async (remoteName: string) => {
    setSubmit({ kind: "busy", stage: "probing" });
    try {
      const budget = await commands.budgetAcquire(remoteName, 1);
      if (budget.status === "error") {
        setSubmit({ kind: "error", message: envelopeError(budget.error) });
      } else if (budget.data !== "allow") {
        setSubmit({ kind: "probe", outcome: aggregateProbe({ budget: budget.data, list: null }) });
      } else {
        const list = await commands.operationsList(`${remoteName}:`);
        if (list.status === "error") {
          setSubmit({ kind: "probe", outcome: aggregateProbe({ budget: "allow", list: { ok: false, error: envelopeError(list.error) } }) });
        } else {
          const parsed = parseDirListing(list.data, "/", remoteName);
          setSubmit({
            kind: "probe",
            outcome: aggregateProbe({ budget: "allow", list: parsed.ok ? { ok: true, entries: parsed.entries.length } : { ok: false, error: parsed.error } }),
          });
        }
      }
    } catch (err) {
      setSubmit({ kind: "error", message: errorText(err) });
    }
  }, []);

  const confirmConnect = useCallback(async () => {
    if (selected === null) return;
    setPreviewOpen(false);
    if (createChannel(selected.branch) === null) {
      // 通道守卫(model):其余分支无已交付 IPC 通道——显式上浮,禁静默/禁伪造成功。
      setSubmit({ kind: "channel", message: t("wizard.channelUnavailable") });
      return;
    }
    const values = form.getValues();
    setSubmit({ kind: "busy", stage: "creating" });
    try {
      const result = await commands.connectionCreate123(asString(values.name), asString(values.endpoint), asString(values.account), asString(values.app_password));
      if (result.status === "error") setSubmit({ kind: "error", message: envelopeError(result.error) });
      else await runProbe(asString(values.name));
    } catch (err) {
      setSubmit({ kind: "error", message: errorText(err) });
    }
  }, [form, runProbe, selected]);

  const groups = useMemo(() => {
    const map = new Map<WizardBranch, ProviderFormOut[]>();
    if (providers.kind === "ready") {
      for (const provider of providers.providers) {
        const branch = routeBranch(provider.name);
        const bucket = map.get(branch);
        if (bucket === undefined) map.set(branch, [provider]);
        else bucket.push(provider);
      }
    }
    return map;
  }, [providers]);

  // 预览行(secret 一律掩码):依赖 previewOpen 以便每次打开时取最新表单值。
  const previewRows = useMemo(() => {
    if (!previewOpen || selected === null) return [];
    const fields = fieldsFor(selected);
    const masked = redactValues(fields, form.getValues());
    return fields.map((field) => ({ label: fieldLabel(field.name), value: masked[field.name] ?? "" }));
  }, [form, previewOpen, selected]);

  return (
    <Dialog.Root open={open} onOpenChange={(next) => { if (!next) close(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/50" />
        <Dialog.Content className="fixed left-1/2 top-1/2 flex max-h-[80vh] w-[520px] -translate-x-1/2 -translate-y-1/2 flex-col rounded-lg border bg-background p-4 shadow-lg">
          <Dialog.Title className="text-sm font-semibold">{t("wizard.title")}</Dialog.Title>
          <Dialog.Description className="mt-0.5 text-xs text-muted-foreground">{t("wizard.subtitle")}</Dialog.Description>
          <ol className="mt-2 flex items-center gap-2 border-b pb-2 text-xs text-muted-foreground" aria-label={t("wizard.stepsLabel")}>
            {STEPS.map((labelKey, index) => (
              <li key={labelKey} aria-current={step === index + 1 ? "step" : undefined} className={step === index + 1 ? "font-medium text-foreground" : ""}>
                {index + 1}. {t(labelKey)}
              </li>
            ))}
          </ol>
          <div className="mt-3 min-h-0 flex-1 overflow-y-auto">
            {step === 1 && (providers.kind === "loading" ? (
              <div aria-busy="true" aria-label={t("wizard.loading")} className="flex flex-col gap-2 p-1">
                {Array.from({ length: 4 }, (_, index) => (
                  <span key={index} className="inline-block h-6 animate-pulse rounded bg-muted" style={{ width: `${70 - index * 9}%` }} />
                ))}
              </div>
            ) : providers.kind === "error" ? (
              <div role="alert" className="flex flex-col items-start gap-2">
                <p className="text-sm text-destructive">{t("wizard.loadError")}</p>
                <p className="break-all text-xs text-muted-foreground">{providers.message}</p>
                <Button variant="outline" size="sm" onClick={() => { setProviders({ kind: "loading" }); void fetchProviderForms().then(setProviders); }}>{t("wizard.retry")}</Button>
              </div>
            ) : providers.providers.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t("wizard.empty")}</p>
            ) : (
              <div className="flex flex-col gap-3">
                {BRANCH_ORDER.filter((branch) => groups.has(branch)).map((branch) => (
                  <section key={branch}>
                    <h3 className="mb-1 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">{t(branchGroupKey(branch))}</h3>
                    <div className="flex flex-wrap gap-1">
                      {(groups.get(branch) ?? []).map((provider) => (
                        <button key={provider.name} type="button" title={provider.description} onClick={() => selectProvider(provider)} className="rounded-md border border-input px-2 py-1 text-xs hover:bg-accent">
                          {provider.name}
                        </button>
                      ))}
                    </div>
                  </section>
                ))}
              </div>
            ))}
            {step === 2 && selected !== null && (
              <ConfigureForm fields={fieldsFor(selected)} branch={selected.branch} providerName={selected.provider.name} form={form}
                onBack={() => setStep(1)}
                onNext={() => { setStep(3); setPreviewOpen(true); }} />
            )}
            {step === 3 && selected !== null && (submit.kind === "idle" ? (
              <p className="text-xs text-muted-foreground">{t("wizard.previewHint")}</p>
            ) : submit.kind === "busy" ? (
              <p aria-busy="true" className="text-sm">{submit.stage === "creating" ? t("wizard.creating") : t("wizard.probe.running")}</p>
            ) : submit.kind === "probe" ? (
              <ProbePanel outcome={submit.outcome} onRetry={() => void runProbe(asString(form.getValues().name))} onDone={close} />
            ) : (
              <div role="alert" className="flex flex-col items-start gap-2">
                <p className="text-sm text-destructive">{submit.kind === "channel" ? t("wizard.channelTitle") : t("wizard.errorTitle")}</p>
                <p className="break-all text-xs text-muted-foreground">{submit.message}</p>
                <Button variant="outline" size="sm" onClick={() => setStep(2)}>{t("wizard.back")}</Button>
              </div>
            ))}
          </div>
          <Dialog.Close asChild>
            <button type="button" className="absolute right-3 top-3 rounded px-1 text-muted-foreground hover:bg-muted" aria-label={t("wizard.close")}>×</button>
          </Dialog.Close>
        </Dialog.Content>
      </Dialog.Portal>
      {/* 预览-提交对话框(DoD②):secret 一律掩码展示,确认才触达创建/探针。 */}
      <Dialog.Root open={previewOpen} onOpenChange={setPreviewOpen}>
        <Dialog.Portal>
          <Dialog.Overlay className="fixed inset-0 bg-black/50" />
          <Dialog.Content className="fixed left-1/2 top-1/2 w-[460px] -translate-x-1/2 -translate-y-1/2 rounded-lg border bg-background p-4 shadow-lg">
            <Dialog.Title className="text-sm font-semibold">{t("wizard.previewTitle")}</Dialog.Title>
            <Dialog.Description className="mt-0.5 text-xs text-muted-foreground">{t("wizard.previewHint")}</Dialog.Description>
            <dl className="mt-3 flex max-h-64 flex-col gap-1.5 overflow-y-auto">
              {previewRows.map((row) => (
                <div key={row.label} className="flex gap-2 text-xs">
                  <dt className="w-40 shrink-0 text-muted-foreground">{row.label}</dt>
                  <dd className="break-all">{row.value}</dd>
                </div>
              ))}
            </dl>
            <div className="mt-4 flex justify-end gap-2">
              <Button variant="outline" size="sm" onClick={() => setPreviewOpen(false)}>{t("wizard.cancel")}</Button>
              <Button size="sm" onClick={() => void confirmConnect()}>{t("wizard.confirmConnect")}</Button>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </Dialog.Root>
  );
}
