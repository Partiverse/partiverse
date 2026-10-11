// 向导纯逻辑单测(DoD⑥):分支路由 / zod schema 生成 / secret 脱敏 / 体检结果聚合 /
// 创建通道守卫 / 文案 key 全部可解析(缺 key 抛错=测试失败)。
import { describe, expect, it } from "vitest";

import { commands, type FieldDesc } from "@/bindings";
import { t } from "@/i18n";
import {
  aggregateProbe, BAIDU_SANDBOX_NOTICE_KEY, branchGroupKey, branchGuidanceKeys, buildDefaultValues,
  buildZodSchema, collectProtocolParameters, createChannel, GDRIVE_DRIVEFILE_NOTICE_KEY, isLocalProvider,
  PAN123_FIELDS, redactValues, routeBranch, SECRET_MASK, type ProbeInput,
} from "./model";

function desc(overrides: Partial<FieldDesc>): FieldDesc {
  return { name: "f", field_type: "string", required: false, is_password: false, advanced: false, exclusive: false, default: null, ...overrides };
}

describe("分支路由", () => {
  it("123 名一律走 pan123(单轨=仅 WebDAV);baidu 走 oob 流", () => {
    expect(routeBranch("123pan")).toBe("pan123");
    expect(routeBranch("baidu_netdisk")).toBe("baidu");
  });

  it("OAuth 清单命中 oauth,协议类与其余 provider 兜底 protocol", () => {
    expect(routeBranch("onedrive")).toBe("oauth");
    expect(routeBranch("drive")).toBe("oauth");
    for (const name of ["smb", "s3", "webdav", "jottacloud"]) expect(routeBranch(name)).toBe("protocol");
  });

  it("分组与引导 key 均可在语言包解析;D7 与百度沙箱 key 必在引导清单", () => {
    for (const branch of ["protocol", "oauth", "baidu", "pan123"] as const) expect(typeof t(branchGroupKey(branch))).toBe("string");
    const keys = [...branchGuidanceKeys("pan123", "123pan"), ...branchGuidanceKeys("baidu", "baidu_netdisk"),
      ...branchGuidanceKeys("oauth", "drive"), ...branchGuidanceKeys("oauth", "onedrive"), ...branchGuidanceKeys("protocol", "smb")];
    expect(keys).toContain(GDRIVE_DRIVEFILE_NOTICE_KEY);
    expect(keys).toContain(BAIDU_SANDBOX_NOTICE_KEY);
    for (const key of keys) expect(typeof t(key)).toBe("string");
  });
});

describe("zod schema 生成", () => {
  it("required 缺失/空串拒绝,可选缺失放行;bool 按布尔校验;字符串裁剪空白", () => {
    const schema = buildZodSchema([desc({ name: "host", required: true }), desc({ name: "port" }), desc({ name: "ro", field_type: "bool" })]);
    expect(schema.safeParse({ host: "h", port: "", ro: false }).success).toBe(true);
    expect(schema.safeParse({ host: "", port: "", ro: false }).success).toBe(false);
    expect(schema.safeParse({ port: "", ro: false }).success).toBe(false);
    expect(schema.safeParse({ host: "h", ro: "yes" }).success).toBe(false);
    expect((schema.parse({ host: " h ", ro: false }) as { host: string }).host).toBe("h");
  });

  it("default(JSON 文本)注入初值;非法 JSON 即抛缺陷", () => {
    expect(buildDefaultValues([desc({ name: "host", default: '"hb"' }), desc({ name: "ro", field_type: "bool", default: "true" })]))
      .toEqual({ host: "hb", ro: true });
    expect(() => buildDefaultValues([desc({ name: "bad", default: "{oops" })])).toThrowError(/invalid provider default JSON/);
  });
});

describe("secret 脱敏(R2 红线 UI 层)", () => {
  it("is_password 一律掩码,原值不可现;布尔序列化文本;缺失落空串", () => {
    const fields = [desc({ name: "user" }), desc({ name: "pass", required: true, is_password: true })];
    const out = redactValues(fields, { user: "alice", pass: "sup3r-secret" });
    expect(out).toEqual({ user: "alice", pass: SECRET_MASK });
    expect(JSON.stringify(out)).not.toContain("sup3r-secret");
    expect(redactValues([desc({ name: "ro", field_type: "bool" }), desc({ name: "note" })], { ro: true })).toEqual({ ro: "true", note: "" });
  });
});

describe("创建通道守卫(M1-WP05-T09 接线)", () => {
  it("pan123=connection_create_123、协议类=connection_create_protocol;oauth/baidu 显式 null", () => {
    expect(createChannel("pan123")).toBe("connection_create_123");
    expect(createChannel("protocol")).toBe("connection_create_protocol");
    for (const branch of ["oauth", "baidu"] as const) expect(createChannel(branch)).toBeNull();
  });

  it("local 后端判定:大小写不敏感命中 local,其余不误伤(webdav/smb)", () => {
    expect(isLocalProvider("local")).toBe(true);
    expect(isLocalProvider("Local")).toBe(true);
    expect(isLocalProvider("webdav")).toBe(false);
    expect(isLocalProvider("smb")).toBe(false);
  });

  it("协议类表单值 → parameters JSON 文本:name 除外、空串/false 落缺省、trim、password 原值直达 IPC", () => {
    const fields: FieldDesc[] = [
      { name: "name", field_type: "string", required: true, is_password: false, advanced: false, exclusive: false, default: null },
      { name: "url", field_type: "string", required: true, is_password: false, advanced: false, exclusive: false, default: null },
      { name: "vendor", field_type: "string", required: false, is_password: false, advanced: false, exclusive: false, default: null },
      { name: "pass", field_type: "string", required: false, is_password: true, advanced: false, exclusive: false, default: null },
      { name: "ro", field_type: "bool", required: false, is_password: false, advanced: false, exclusive: false, default: null },
    ];
    const json = collectProtocolParameters(fields, {
      name: "mywebdav",
      url: "  https://dav.example.invalid/  ",
      vendor: "",
      pass: "s3cr3t-pass-value",
      ro: true,
    });
    const parsed = JSON.parse(json) as Record<string, unknown>;
    expect(parsed).toEqual({
      url: "https://dav.example.invalid/",
      pass: "s3cr3t-pass-value",
      ro: true,
    });
    // name 不入 parameters(remote 名走 config/create 顶层参数);空串不落键。
    expect("name" in parsed).toBe(false);
    expect("vendor" in parsed).toBe(false);
  });

  it("123 专用表单:字段序固定、全必填、密码字段=app_password", () => {
    expect(PAN123_FIELDS.map((f) => f.name)).toEqual(["name", "endpoint", "account", "app_password"]);
    expect(PAN123_FIELDS.every((f) => f.required)).toBe(true);
    expect(PAN123_FIELDS.find((f) => f.is_password)?.name).toBe("app_password");
  });
});

describe("百度换码通道过渡契约(M1-WP05-T08 Owner 裁定)", () => {
  it("壳命令 baidu_exchange_code 已入 bindings(T01 口径入库,签名四参数全前端注入)", () => {
    expect(typeof commands.baiduExchangeCode).toBe("function");
  });

  it("向导守卫在 backends-go 百度后端落地前不接 baidu 换码(接线必徒耗一次性 code,保持显式上浮)", () => {
    expect(createChannel("baidu")).toBeNull();
    expect(createChannel("baidu")).not.toBe("baidu_exchange_code");
  });
});

describe("连接体检结果聚合", () => {
  it("成功:实测能力位 ok,WP09 项标 reserved 不冒充", () => {
    const outcome = aggregateProbe({ budget: "allow", list: { ok: true, entries: 7 } } satisfies ProbeInput);
    expect(outcome.status).toBe("ok");
    expect(outcome.capabilityBits.filter((bit) => bit.state === "ok").map((bit) => bit.key))
      .toEqual(["wizard.probe.capList", "wizard.probe.capBudget"]);
    expect(outcome.capabilityBits.some((bit) => bit.state === "reserved" && bit.key === "wizard.probe.capHash")).toBe(true);
    expect(outcome.diagnosticsKeys).toEqual([]);
  });

  it("throttled 携带可重试时刻且未触引擎(list=null)", () => {
    const outcome = aggregateProbe({ budget: { throttled: { wait_until_ms: 1234 } }, list: null });
    expect(outcome.status).toBe("throttled");
    expect(outcome.waitUntilMs).toBe(1234);
  });

  it("exhausted 与列举失败给出诊断步骤 key(全部可解析)", () => {
    const exhausted = aggregateProbe({ budget: "exhausted", list: null });
    expect(exhausted.status).toBe("error");
    expect(exhausted.diagnosticsKeys).toEqual(["wizard.probe.diagBudget"]);
    const failed = aggregateProbe({ budget: "allow", list: { ok: false, error: "connection refused" } });
    expect(failed.status).toBe("error");
    expect(failed.error).toBe("connection refused");
    expect(failed.diagnosticsKeys.length).toBeGreaterThanOrEqual(3);
    for (const key of [...exhausted.diagnosticsKeys, ...failed.diagnosticsKeys]) expect(typeof t(key)).toBe("string");
  });
});
