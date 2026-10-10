// ESLint 工程护栏(M1-WP05-T01;docs/08-前端UIUX规划.md §8)。
// 「jsx 安全插值规则」= partisync ui_hardening 方法论 ESLint 化:React 对 JSX
// 插值默认转义,护栏封死全部 HTML 逃逸口(dangerouslySetInnerHTML /
// innerHTML 写入 / document.write);react-hooks/recommended 守 hooks 纪律;
// package.json 的 lint 脚本挂 --max-warnings 0 构成零 warning 门。
// 生成物与产物不入检查面:src/bindings.ts(specta 生成)、dist/、src-tauri/
// (Rust 侧产物由 src-tauri 三门禁自守,见运行报告)。
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist/", "src/bindings.ts", "src-tauri/"] },
  tseslint.configs.recommended,
  reactHooks.configs.flat.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    rules: {
      "no-restricted-syntax": [
        "error",
        {
          selector: "JSXAttribute[name.name='dangerouslySetInnerHTML']",
          message: "禁止 dangerouslySetInnerHTML:动态内容一律走 React 转义插值。",
        },
        {
          selector: "MemberExpression[property.name=/^(innerHTML|outerHTML)$/]",
          message: "禁止 innerHTML/outerHTML 写入(逃逸 React 转义通道)。",
        },
        {
          selector: "CallExpression[callee.object.name='document'][callee.property.name='write']",
          message: "禁止 document.write(逃逸 React 转义通道)。",
        },
      ],
    },
  },
);
