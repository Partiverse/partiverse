// 引擎「最近错误」微存储(DoD⑤):模块级单例 + useSyncExternalStore。
// 健康徽标数据 = engine_status 快照 + 最近错误(此处);禁吞错。
import { useSyncExternalStore } from "react";

let lastError: string | null = null;
const listeners = new Set<() => void>();

/** 记录最近一次引擎面错误(相同消息去重,防轮询风暴刷订阅)。 */
export function recordEngineError(message: string): void {
  if (lastError === message) return;
  lastError = message;
  for (const listener of listeners) listener();
}

/** React 订阅面:返回最近错误文本(无错误为 null)。 */
export function useLastEngineError(): string | null {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => lastError,
    () => lastError,
  );
}
