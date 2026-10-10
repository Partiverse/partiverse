// 应用壳入口:侧栏/工具行/传输条骨架 + 浏览视图 + 占位路由(browse/诊断,DoD⑤)。
// M1-WP05-T01:specta 强类型 IPC;T02:诊断面板占位路由(仅快照+最近错误)。
import { useEffect, useState } from "react";
import { commands } from "@/bindings";
import { Sidebar } from "@/components/shell/Sidebar";
import { Toolbar, type IpcState } from "@/components/shell/Toolbar";
import { useEngineStatus } from "@/components/shell/EngineHealthBadge";
import { BrowseView } from "@/components/browse/BrowseView";
import { TransferBar } from "@/components/shell/TransferBar";
import { useLastEngineError } from "@/lib/healthStore";
import { t } from "@/i18n";

type Route = "browse" | "diagnostics";

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-2 text-sm">
      <dt className="w-24 shrink-0 text-muted-foreground">{label}</dt>
      <dd className="truncate text-foreground">{value}</dd>
    </div>
  );
}

function DiagnosticsPanel({ onBack }: { onBack: () => void }) {
  const snapshot = useEngineStatus();
  const lastError = useLastEngineError();
  const none = t("diagnostics.none");
  const unreachable = snapshot === "error";
  const text = (value: string | null | undefined) => value ?? none;
  return (
    <main className="flex min-h-0 flex-1 flex-col p-4">
      <div className="mb-3 flex items-center gap-2">
        <h1 className="text-sm font-semibold">{t("diagnostics.title")}</h1>
        <button type="button" className="ms-auto rounded px-2 py-1 text-xs hover:bg-muted" onClick={onBack}>{t("diagnostics.back")}</button>
      </div>
      <dl className="flex max-w-md flex-col gap-2">
        <Row label={t("diagnostics.slot")} value={unreachable ? t("health.state.unreachable") : text(snapshot?.slot_id)} />
        <Row label={t("diagnostics.state")} value={unreachable ? t("health.state.unreachable") : text(snapshot?.state)} />
        <Row label={t("diagnostics.pid")} value={snapshot === null || unreachable ? none : text(snapshot.pid?.toString())} />
        <Row label={t("diagnostics.socket")} value={snapshot === null || unreachable ? none : text(snapshot.socket_path)} />
        <Row label={t("diagnostics.lastError")} value={lastError ?? none} />
      </dl>
    </main>
  );
}

function App() {
  const [ipc, setIpc] = useState<IpcState>({ kind: "loading" });
  const [route, setRoute] = useState<Route>("browse");

  useEffect(() => {
    let cancelled = false;
    // 命令错误以 {status:"error",error:CmdError} 信封返回,传输层异常以 Error 抛出,
    // 两条失败路径都必须上浮 UI(禁止吞错)。
    commands
      .appVersion()
      .then((result) => {
        if (cancelled) return;
        if (result.status === "ok") setIpc({ kind: "ok", version: result.data });
        else setIpc({ kind: "error", message: result.error.msg });
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setIpc({ kind: "error", message: err instanceof Error ? err.message : JSON.stringify(err) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground">
      <div className="flex min-h-0 flex-1">
        <Sidebar onOpenDiagnostics={() => setRoute("diagnostics")} />
        <div className="flex min-w-0 flex-1 flex-col">
          <Toolbar ipc={ipc} />
          {route === "browse" ? <BrowseView /> : <DiagnosticsPanel onBack={() => setRoute("browse")} />}
          <TransferBar />
        </div>
      </div>
    </div>
  );
}

export default App;
