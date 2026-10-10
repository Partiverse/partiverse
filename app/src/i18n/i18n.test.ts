// i18n 纯逻辑单测(M1-WP05-T01 DoD④:查表契约——缺失键必须抛错,禁止静默回退)。
import { describe, expect, it } from "vitest";

import { t } from "./index";

describe("i18n 查表", () => {
  it("已知键返回英文文案", () => {
    expect(t("app.title")).toBe("Partiverse");
  });

  it("缺失键立即抛错(无静默回退)", () => {
    expect(() => t("nope.missing")).toThrowError(/missing i18n key: nope\.missing/);
  });
});
