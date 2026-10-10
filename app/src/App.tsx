// 应用壳入口:静态骨架组装(侧栏/工具行/内容区/底部传输条)+ 示范 IPC(app_version)。
// M1-WP05-T01:示范 IPC 切换为 specta 生成绑定(src/bindings.ts,强类型命令面)。
import { useEffect, useState } from "react";
import { commands } from "@/bindings";
import { Sidebar } from "@/components/shell/Sidebar";
import { Toolbar, type IpcState } from "@/components/shell/Toolbar";
import { ContentArea } from "@/components/shell/ContentArea";
import { TransferBar } from "@/components/shell/TransferBar";

function App() {
  const [ipc, setIpc] = useState<IpcState>({ kind: "loading" });

  useEffect(() => {
    let cancelled = false;
    // 生成绑定调用:命令错误以 {status:"error",error:CmdError} 信封返回,
    // 传输层异常以 Error 抛出——两条失败路径都必须上浮 UI(禁止吞错)。
    commands
      .appVersion()
      .then((result) => {
        if (cancelled) return;
        if (result.status === "ok") {
          setIpc({ kind: "ok", version: result.data });
        } else {
          setIpc({ kind: "error", message: result.error.msg });
        }
      })
      .catch((err: unknown) => {
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
