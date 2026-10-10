// 设计令牌挂载:tokens.json 是品牌变量的唯一事实源,启动时写入 :root(CSSOM,
// 不受 CSP style-src 限制);CSS 侧仅 var() 间接引用(@theme inline)防双源漂移。
// 主题三套(DoD⑦)= html[data-theme] 标记 + .dark 类:暗沿用 index.css 既有 .dark
// 表(暗默认),亮=缺省 :root 表,暖=[data-theme="warm"] 表;品牌令牌仍单源自 tokens.json。
import tokens from "./tokens.json";

type CssVars = Record<string, string>;

function collectVars(node: unknown, out: CssVars): void {
  if (node === null || typeof node !== "object") return;
  for (const [key, value] of Object.entries(node)) {
    if (typeof value === "string") {
      if (key.startsWith("--")) out[key] = value;
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

export type ThemeName = "dark" | "light" | "warm";
export const THEME_ORDER: ThemeName[] = ["dark", "light", "warm"];

/** 应用主题:写 data-theme 并同步 .dark 类(语义表随 CSS 级联切换)。 */
export function applyTheme(name: ThemeName, root: HTMLElement = document.documentElement): void {
  root.dataset.theme = name;
  root.classList.toggle("dark", name === "dark");
}
