import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyDesignTokens } from "@/design/tokens";
import "@/index.css";

// 先挂设计令牌再渲染,保证首帧即拿到品牌变量(深色为默认基调,html.dark 见 index.html)。
applyDesignTokens();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
