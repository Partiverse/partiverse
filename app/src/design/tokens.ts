// 设计令牌挂载:tokens.json 是品牌/语义变量的唯一事实源,启动时写入 :root。
// CSS 侧只通过 var() 间接引用(@theme inline),防止 JSON 与 CSS 双源漂移。
// 挂载走 CSSOM(setProperty),不受 CSP style-src 限制。
import tokens from "./tokens.json";

type CssVars = Record<string, string>;

function isCssVarName(key: string): boolean {
  return key.startsWith("--");
}

function collectVars(node: unknown, out: CssVars): void {
  if (typeof node === "string") return; // 字符串仅在对象层级收集(见下)
  if (node === null || typeof node !== "object") return;
  for (const [key, value] of Object.entries(node)) {
    if (typeof value === "string") {
      if (isCssVarName(key)) out[key] = value;
    } else {
      collectVars(value, out);
    }
  }
}

/** 汇总 tokens.json 中全部 CSS 变量(忽略 $meta 等注记键)。 */
export function designTokenVars(): CssVars {
  const out: CssVars = {};
  collectVars(tokens, out);
  return out;
}

/** 把设计令牌挂载到指定根元素(默认 document.documentElement)。 */
export function applyDesignTokens(root: HTMLElement = document.documentElement): void {
  for (const [name, value] of Object.entries(designTokenVars())) {
    root.style.setProperty(name, value);
  }
}
