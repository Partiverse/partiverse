// 向导创建通道接线行为测试(M1-WP05-T09 DoD⑤ 无人值守轮,08 §4.4):守卫移除后
// 通道真调——协议类分支提交即真调 connection_create_protocol(name/type/parameters
// JSON 形状钉住,secret 原值仅过 IPC 形参不落日志面)、local 分支免 config 直探
// 家目录(user_home_dir→operationsList 裸路径 fs+remote 空串);oauth/baidu 守卫
// 维持显式上浮(T08 裁定)。Fake* 纪律:零真实 IPC。待 Owner GUI 实操复测
// (添加 local/WebDAV→浏览→新建文件夹全链),非静默标绿。
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AddNodeWizard } from "./AddNodeWizard";
import type { FieldDesc, ProviderFormOut } from "@/bindings";

const fakes = vi.hoisted(() => ({
  providersFetchCalls: [] as unknown[][],
  connectionCreate123Calls: [] as unknown[][],
  connectionCreateProtocolCalls: [] as unknown[][],
  userHomeDirCalls: [] as unknown[][],
  budgetAcquireCalls: [] as unknown[][],
  operationsListCalls: [] as unknown[][],
  providersReply: null as unknown,
  /** 协议类通道被拒桩(非 null 时 FakeConnectionCreateProtocol 返回错误信封)。 */
  protocolError: null as unknown,
}));

function fakeField(overrides: Partial<FieldDesc>): FieldDesc {
  return { name: "f", field_type: "string", required: false, is_password: false, advanced: false, exclusive: false, default: null, ...overrides };
}

function fakeProvider(overrides: Partial<ProviderFormOut>): ProviderFormOut {
  return { name: "webdav", description: "WebDAV", fields: [], ...overrides };
}

vi.mock("@/bindings", () => ({
  commands: {
    providersFetch: async () => {
      fakes.providersFetchCalls.push([]);
      if (fakes.providersReply === null) throw new Error("FakeProvidersFetch: reply not staged");
      return { status: "ok", data: fakes.providersReply as ProviderFormOut[] };
    },
    connectionCreate123: async (...args: unknown[]) => {
      fakes.connectionCreate123Calls.push(args);
      return { status: "ok", data: null };
    },
    connectionCreateProtocol: async (...args: unknown[]) => {
      fakes.connectionCreateProtocolCalls.push(args);
      if (fakes.protocolError !== null) return { status: "error", error: fakes.protocolError };
      return { status: "ok", data: null };
    },
    userHomeDir: async () => {
      fakes.userHomeDirCalls.push([]);
      return { status: "ok", data: "/home/fake-user" };
    },
    budgetAcquire: async (...args: unknown[]) => {
      fakes.budgetAcquireCalls.push(args);
      return { status: "ok", data: "allow" };
    },
    operationsList: async (...args: unknown[]) => {
      fakes.operationsListCalls.push(args);
      return { status: "ok", data: JSON.stringify({ list: [] }) };
    },
  },
}));

afterEach(() => {
  cleanup();
  // Fake 记录逐用例清零(禁跨用例串扰);未清的桩标记显式复位。
  fakes.providersFetchCalls.length = 0;
  fakes.connectionCreate123Calls.length = 0;
  fakes.connectionCreateProtocolCalls.length = 0;
  fakes.userHomeDirCalls.length = 0;
  fakes.budgetAcquireCalls.length = 0;
  fakes.operationsListCalls.length = 0;
  fakes.providersReply = null;
  fakes.protocolError = null;
});

const WEBDAV_PROVIDER = fakeProvider({
  name: "webdav",
  fields: [fakeField({ name: "url", required: true }), fakeField({ name: "pass", is_password: true })],
});
const LOCAL_PROVIDER = fakeProvider({ name: "local", description: "Local Disk", fields: [fakeField({ name: "nounc", field_type: "bool", advanced: true })] });

function renderWizard(): void {
  render(<AddNodeWizard open onClose={() => undefined} />);
}

async function selectProvider(name: string): Promise<void> {
  fireEvent.click(await screen.findByText(name));
  await waitFor(() => expect(screen.getByLabelText("Remote name")).toBeTruthy());
}

describe("AddNodeWizard 协议类接线(T09 守卫移除后通道真调)", () => {
  it("webdav 提交:真调 connection_create_protocol(name/type/parameters 形状)+remote 根探针", async () => {
    fakes.providersReply = [WEBDAV_PROVIDER];
    renderWizard();
    await selectProvider("webdav");
    fireEvent.change(screen.getByLabelText("Remote name"), { target: { value: "mywebdav" } });
    fireEvent.change(screen.getByLabelText("url"), { target: { value: "https://dav.example.invalid/" } });
    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "s3cr3t-pass-value" } });
    fireEvent.click(screen.getByText("Next"));
    // 预览-提交对话框:secret 一律掩码(原值禁现),确认才触达创建。
    expect(await screen.findByText("Review before connecting")).toBeTruthy();
    expect(screen.queryByText("s3cr3t-pass-value")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(screen.getByText("Connection check passed.")).toBeTruthy());
    // 通道真调形状:parameters JSON 文本(name 除外、非空串原值、含 password 字段)。
    expect(fakes.connectionCreate123Calls).toHaveLength(0);
    expect(fakes.connectionCreateProtocolCalls).toEqual([
      ["mywebdav", "webdav", JSON.stringify({ url: "https://dav.example.invalid/", pass: "s3cr3t-pass-value" })],
    ]);
    // remote 根探针:operationsList 两键齐传(remote 空串合法,T09 实测形状)。
    expect(fakes.budgetAcquireCalls).toEqual([["mywebdav", 1]]);
    expect(fakes.operationsListCalls).toEqual([["mywebdav:", ""]]);
  });

  it("local 分支免 config:跳过创建直探家目录(裸路径 fs + remote 空串)", async () => {
    fakes.providersReply = [LOCAL_PROVIDER];
    renderWizard();
    fireEvent.click(await screen.findByText("local"));
    // 选择即直入 Review(免 config,预览零行),确认后零创建通道触达。
    expect(await screen.findByText("Review before connecting")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(screen.getByText("Connection check passed.")).toBeTruthy());
    expect(fakes.connectionCreateProtocolCalls).toHaveLength(0);
    expect(fakes.connectionCreate123Calls).toHaveLength(0);
    expect(fakes.userHomeDirCalls).toHaveLength(1);
    expect(fakes.budgetAcquireCalls).toEqual([["local", 1]]);
    expect(fakes.operationsListCalls).toEqual([["/home/fake-user", ""]]);
  });

  it("创建通道被拒:错误上浮零伪造成功(禁吞错)", async () => {
    fakes.providersReply = [WEBDAV_PROVIDER];
    fakes.protocolError = { kind: "core", msg: "config/create rejected", severity: "fatal" };
    try {
      renderWizard();
      await selectProvider("webdav");
      fireEvent.change(screen.getByLabelText("Remote name"), { target: { value: "mywebdav" } });
      fireEvent.change(screen.getByLabelText("url"), { target: { value: "https://dav.example.invalid/" } });
      fireEvent.click(screen.getByText("Next"));
      fireEvent.click(await screen.findByRole("button", { name: "Connect" }));
      await waitFor(() => expect(screen.getByRole("alert")).toBeTruthy());
      expect(screen.getByText("Failed to create the remote")).toBeTruthy();
    } finally {
      fakes.protocolError = null;
    }
    // 拒绝路径零探针触达(零伪造成功,错误原样上浮 UI)。
    expect(fakes.operationsListCalls).toHaveLength(0);
  });
});
