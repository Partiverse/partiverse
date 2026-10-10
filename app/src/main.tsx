import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyDesignTokens, applyTheme } from "@/design/tokens";
import "@/index.css";

// 先挂设计令牌与默认主题(暗,DoD⑦)再渲染,保证首帧即拿到品牌/语义变量。
// index.html 的 html.dark 为无闪兜底,applyTheme 落 [data-theme] 标记。
applyDesignTokens();
applyTheme("dark");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
