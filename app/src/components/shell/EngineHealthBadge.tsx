// 侧栏引擎健康徽标(DoD⑤):常驻绿/黄/红三态;数据=engine_status 快照+最近错误;
// 点击=诊断面板占位路由。轮询 10s,挂载即测,失败不吞(入最近错误)。
import { useEffect, useState } from "react";
import { commands, type EngineSnapshot } from "@/bindings";
import { recordEngineError, useLastEngineError } from "@/lib/healthStore";
import { t } from "@/i18n";

export type HealthLevel = "green" | "yellow" | "red";

export interface EngineHealth {
  level: HealthLevel;
  stateKey: string;
}

/** 纯推导(单测直测):快照/错误 → 三态。 */
export function deriveEngineHealth(snapshot: EngineSnapshot | null | "error", hasLastError: boolean): EngineHealth {
  if (snapshot === "error") return { level: "red", stateKey: "health.state.unreachable" };
  if (snapshot === null) {
    return hasLastError
      ? { level: "red", stateKey: "health.state.notRunning" }
      : { level: "yellow", stateKey: "health.state.notRunning" };
  }
  switch (snapshot.state) {
    case "ready":
      return { level: "green", stateKey: "health.state.ready" };
    case "failed":
      return { level: "red", stateKey: "health.state.failed" };
    default: // starting/stopping/exited:过渡或已退出,黄色观望
      return { level: "yellow", stateKey: `health.state.${snapshot.state}` };
  }
}

const LEVEL_DOT: Record<HealthLevel, string> = {
  green: "bg-brand-500",
  yellow: "bg-yellow-500",
  red: "bg-destructive",
};

/** 轮询 engine_status(零引擎触达观测面);诊断面板复用本 hook。 */
export function useEngineStatus(): EngineSnapshot | null | "error" {
  const [snapshot, setSnapshot] = useState<EngineSnapshot | null | "error">(null);
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const result = await commands.engineStatus();
        if (cancelled) return;
        if (result.status === "ok") {
          setSnapshot(result.data);
        } else {
          setSnapshot("error");
          recordEngineError(result.error.msg);
        }
      } catch (err) {
        if (cancelled) return;
        setSnapshot("error");
        recordEngineError(err instanceof Error ? err.message : JSON.stringify(err));
      }
    };
    void tick();
    const timer = setInterval(() => void tick(), 10_000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);
  return snapshot;
}

export function EngineHealthBadge({ onOpen }: { onOpen: () => void }) {
  const snapshot = useEngineStatus();
  const lastError = useLastEngineError();
  const health = deriveEngineHealth(snapshot, lastError !== null);
  return (
    <button
      type="button"
      onClick={onOpen}
      title={lastError ?? t(health.stateKey)}
      aria-label={`${t("health.engine")}: ${t(health.stateKey)}`}
      className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-sm text-sidebar-foreground/90 hover:bg-sidebar-accent"
    >
      <span aria-hidden className={`size-2 rounded-full ${LEVEL_DOT[health.level]}`} />
      <span className="flex-1 text-left">{t("health.engine")}</span>
      <span className="text-xs text-muted-foreground">{t(health.stateKey)}</span>
    </button>
  );
}
