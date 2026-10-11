// 添加 Node 向导纯逻辑层(DoD①③⑥,零 React/IPC 依赖):provider 分支路由、zod schema
// 动态生成(消费 WP04 providers_fetch 描述模型)、secret 脱敏(R2 零明文红线在 UI 层延续:
// 不入日志/localStorage,预览一律掩码)、轻探针体检结果聚合、创建通道守卫。
// 文案 key 常量(D7 与百度沙箱)由此导出,组件只消费。
import { z } from "zod";
import type { BudgetDecision, FieldDesc } from "@/bindings";

/** 百度沙箱提示文案 key(DoD①指定 key:BAIDU_SANDBOX_NOTICE)。 */
export const BAIDU_SANDBOX_NOTICE_KEY = "wizard.baiduSandboxNotice";
/** GDrive `drive.file` 限制明示引导文案 key(✅D7,SPEC §2.2)。 */
export const GDRIVE_DRIVEFILE_NOTICE_KEY = "wizard.gdriveDrivefileNotice";
/** secret 字段展示掩码(预览/日志同源,禁止出现原值)。 */
export const SECRET_MASK = "••••••••";

export type WizardBranch = "protocol" | "oauth" | "baidu" | "pan123";

/** OAuth 类清单(08 规划 §4.1);清单外 provider 一律按协议类 schema 表单兜底。 */
const OAUTH_PROVIDERS = new Set(["onedrive", "drive", "gphotos", "dropbox", "box", "pcloud", "mega"]);

/** 分支路由(DoD⑥):123 单轨=仅官方 WebDAV(SPEC §2.2 修订),名字命中 123 一律走
 *  pan123 专用流(拒其 OpenAPI schema);baidu 走 oob 回填流。 */
export function routeBranch(providerName: string): WizardBranch {
  const name = providerName.toLowerCase();
  if (name.includes("123")) return "pan123";
  if (name.includes("baidu")) return "baidu";
  if (OAUTH_PROVIDERS.has(name)) return "oauth";
  return "protocol";
}

export const branchGroupKey = (branch: WizardBranch): string => `wizard.group${branch[0].toUpperCase()}${branch.slice(1)}`;

/** 分支引导文案 key(有序;组件按序渲染,测试钉住 key 均可解析)。 */
export function branchGuidanceKeys(branch: WizardBranch, providerName: string): string[] {
  if (branch === "pan123") return ["wizard.123.endpointHint", "wizard.123.passwordHint", "wizard.123.vipNotice"];
  if (branch === "baidu") return ["wizard.baidu.appKeyHint", "wizard.baidu.codeHint", BAIDU_SANDBOX_NOTICE_KEY];
  if (branch === "oauth") {
    return providerName.toLowerCase() === "drive"
      ? [GDRIVE_DRIVEFILE_NOTICE_KEY, "wizard.oauth.browserHint"]
      : ["wizard.oauth.browserHint"];
  }
  return [];
}

// bool → 布尔(表单勾选框);其余类型一律表单字符串(text/password 同源)。zod 只
// 承诺「required=非空、可选=可缺省」,不发明 rclone 字段类型语义;可缺省字段允许
// 键缺席(undefined)——表单初值恒经 buildDefaultValues 补齐,双保险。
function schemaForField(field: FieldDesc): z.ZodType {
  if (field.field_type === "bool") return field.required ? z.boolean() : z.boolean().optional();
  const base = z.string().trim();
  return field.required ? base.min(1) : base.optional();
}

/** DoD⑥:providers_fetch 字段描述 → zod schema(required=非空,bool=布尔)。 */
export function buildZodSchema(fields: FieldDesc[]): z.ZodObject {
  const shape: Record<string, z.ZodType> = {};
  for (const field of fields) shape[field.name] = schemaForField(field);
  return z.object(shape);
}

/** default(JSON 文本)解析;失败即抛缺陷(禁静默造默认值)。 */
function parseDefaultJson(json: string, name: string): unknown {
  try {
    return JSON.parse(json) as unknown;
  } catch {
    throw new Error(`invalid provider default JSON for field "${name}"`);
  }
}

/** 表单初值:default(JSON 文本,null=无)→ bool=false、string=""。 */
export function buildDefaultValues(fields: FieldDesc[]): Record<string, string | boolean> {
  const values: Record<string, string | boolean> = {};
  for (const field of fields) {
    if (field.field_type === "bool") {
      values[field.name] = field.default !== null ? parseDefaultJson(field.default, field.name) === true : false;
    } else {
      values[field.name] = field.default !== null ? String(parseDefaultJson(field.default, field.name)) : "";
    }
  }
  return values;
}

/** secret 脱敏:is_password 字段输出掩码常量;预览与任何日志必须经此函数。 */
export function redactValues(fields: FieldDesc[], values: Record<string, unknown>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const field of fields) {
    if (field.is_password) {
      out[field.name] = SECRET_MASK;
      continue;
    }
    const raw = values[field.name];
    out[field.name] = typeof raw === "boolean" ? (raw ? "true" : "false") : String(raw ?? "");
  }
  return out;
}

/** 创建通道守卫(M1-WP05-T09 更新):pan123=connection_create_123、协议类=
 *  connection_create_protocol(T09 新建壳命令:config/create 透传+config/get
 *  回读断言);oauth/baidu 维持 null(T08 Owner 裁定 2026-10-10:baidu 引擎钉定
 *  1.75.1 无后端,接线必徒耗一次性授权 code;oauth 类换码流属另卡),调用方
 *  必须显式上浮「通道未交付」错误态(禁静默/禁伪造成功)。 */
export type CreateChannelCommand = "connection_create_123" | "connection_create_protocol";
export function createChannel(branch: WizardBranch): CreateChannelCommand | null {
  if (branch === "pan123") return "connection_create_123";
  if (branch === "protocol") return "connection_create_protocol";
  return null;
}

/** local 后端判定(DoD④ local 分支):2026-10-11 实机锚定 rclone 1.75.1
 *  `config/providers` local 条目仅含 advanced 可选项(零必填零密码),免
 *  config 直浏览家目录;判定独立成纯函数供测试钉住。 */
export function isLocalProvider(providerName: string): boolean {
  return providerName.toLowerCase() === "local";
}

/** 协议类表单值 → config/create parameters(JSON 文本,T09 DoD① specta 口径):
 *  `name` 除外(rclone remote 名走 config/create 顶层 name 参数,非 options 项);
 *  空串/false 落缺省(不覆盖 backend 默认,实测空串可覆盖枚举候选语义);
 *  bool true / 非空串(含 password 字段)原值入参——secret 仅经 IPC 通道直达
 *  壳 connection_create_protocol,零日志零预览(预览面仍走 redactValues 掩码)。 */
export function collectProtocolParameters(fields: FieldDesc[], values: Record<string, unknown>): string {
  const out: Record<string, unknown> = {};
  for (const field of fields) {
    if (field.name === "name") continue;
    const raw = values[field.name];
    if (field.field_type === "bool") {
      if (raw === true) out[field.name] = true;
    } else if (typeof raw === "string" && raw.trim() !== "") {
      out[field.name] = raw.trim();
    }
  }
  return JSON.stringify(out);
}

function field(name: string, required: boolean, isPassword: boolean): FieldDesc {
  return { name, field_type: "string", required, is_password: isPassword, advanced: false, exclusive: false, default: null };
}

/** 123 官方 WebDAV 专用表单(端点零硬编码=用户从控制台粘贴;VIP 权益文案见 i18n)。 */
export const PAN123_FIELDS: FieldDesc[] = [
  field("name", true, false),
  field("endpoint", true, false),
  field("account", true, false),
  field("app_password", true, true),
];

/** 百度 oob 流表单(AppKey 用户侧注入,WP04 口径;code=授权页展示后回填)。 */
export const BAIDU_FIELDS: FieldDesc[] = [
  field("name", true, false),
  field("client_id", true, false),
  field("client_secret", true, true),
  field("code", true, false),
];

export interface ProbeInput {
  budget: BudgetDecision;
  /** 预算放行后才执行 operations/list 单次;throttled/exhausted 时为 null(零引擎触达)。 */
  list: { ok: true; entries: number } | { ok: false; error: string } | null;
}

export interface CapabilityBit {
  key: string;
  state: "ok" | "reserved";
}

export interface ProbeOutcome {
  status: "ok" | "throttled" | "error";
  waitUntilMs: number | null;
  error: string | null;
  capabilityBits: CapabilityBit[];
  diagnosticsKeys: string[];
}

/** DoD③⑥:轻探针结果聚合——预算 acquire 前置 + operations/list 一次;能力位只列实测
 *  可得项,哈希/直链时效/配额属 WP09 静态 caps,标 reserved 不冒充;失败给诊断步骤。 */
export function aggregateProbe(input: ProbeInput): ProbeOutcome {
  const okBits: CapabilityBit[] = [
    { key: "wizard.probe.capList", state: "ok" },
    { key: "wizard.probe.capBudget", state: "ok" },
    { key: "wizard.probe.capHash", state: "reserved" },
    { key: "wizard.probe.capDirectLink", state: "reserved" },
    { key: "wizard.probe.capQuota", state: "reserved" },
  ];
  if (typeof input.budget === "object" && "throttled" in input.budget) {
    return { status: "throttled", waitUntilMs: input.budget.throttled.wait_until_ms, error: null, capabilityBits: [], diagnosticsKeys: [] };
  }
  if (input.budget === "exhausted") {
    return { status: "error", waitUntilMs: null, error: null, capabilityBits: [], diagnosticsKeys: ["wizard.probe.diagBudget"] };
  }
  if (input.list === null) {
    return { status: "error", waitUntilMs: null, error: "probe list missing after budget admission", capabilityBits: [], diagnosticsKeys: ["wizard.probe.diagEngine"] };
  }
  if (!input.list.ok) {
    return {
      status: "error", waitUntilMs: null, error: input.list.error, capabilityBits: [],
      diagnosticsKeys: ["wizard.probe.diagEndpoint", "wizard.probe.diagCredentials", "wizard.probe.diagEngine"],
    };
  }
  return { status: "ok", waitUntilMs: null, error: null, capabilityBits: okBits, diagnosticsKeys: [] };
}
