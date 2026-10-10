// ⌘K「文件」区数据源(DoD④:搜文件=当前目录):useBrowse 列举成功后发布,面板订阅。
// 模块级单例 + useSyncExternalStore(同 healthStore 模式);零 localStorage、零日志。
import { useSyncExternalStore } from "react";
import type { PaletteEntry } from "./index";

export interface PaletteSource {
  nodeLabel: string;
  path: string;
  files: PaletteEntry[];
}

let source: PaletteSource = { nodeLabel: "", path: "/", files: [] };
const listeners = new Set<() => void>();

export function publishPaletteSource(next: PaletteSource): void {
  source = next;
  for (const listener of listeners) listener();
}

export function usePaletteSource(): PaletteSource {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    () => source,
    () => source,
  );
}
