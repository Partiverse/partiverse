// 唯一语言包 = English(AGENTS.md GUI 语言一致性红线;day-one 资源化,08 规划 §10.7)。
// 不引入 i18n 框架依赖:点路径查表,缺失键直接抛错(禁止吞错/静默回退)。
import en from "./en.json";

const table: Record<string, string> = en;

/** 按点路径取英文文案;键缺失视为缺陷,立即抛错上浮。 */
export function t(key: string): string {
  const value = table[key];
  if (typeof value !== "string") {
    throw new Error(`missing i18n key: ${key}`);
  }
  return value;
}
