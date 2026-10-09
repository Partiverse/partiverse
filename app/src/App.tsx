// 应用壳入口:静态骨架组装(侧栏/工具行/内容区/底部传输条)+ 示范 IPC(app_version)。
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Sidebar } from "@/components/shell/Sidebar";
import { Toolbar, type IpcState } from "@/components/shell/Toolbar";
import { ContentArea } from "@/components/shell/ContentArea";
import { TransferBar } from "@/components/shell/TransferBar";

function App() {
  const [ipc, setIpc] = useState<IpcState>({ kind: "loading" });

  useEffect(() => {
    let cancelled = false;
    // 示范 IPC: specta 管道(见 src-tauri/src/lib.rs)在系统 GUI 依赖就绪后
    // 经 `cargo test` 导出强类型 bindings.ts,届时此处切换为生成绑定。
    invoke<string>("app_version")
      .then((version) => {
        if (!cancelled) setIpc({ kind: "ok", version });
      })
      .catch((err: unknown) => {
        // 错误必须上浮 UI(禁止吞错):非 Error 的 IPC 形状 {kind,msg} 序列化展示。
        if (cancelled) return;
        const message = err instanceof Error ? err.message : JSON.stringify(err);
        setIpc({ kind: "error", message });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground">
      <div className="flex min-h-0 flex-1">
        <Sidebar />
        <div className="flex min-w-0 flex-1 flex-col">
          <Toolbar ipc={ipc} />
          <ContentArea />
          <TransferBar />
        </div>
      </div>
    </div>
  );
}

export default App;
