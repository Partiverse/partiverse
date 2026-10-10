// Toolbar 行为单测(M1-WP05-T01 DoD④:testing-library 行为面;视觉断言不做)。
// 覆盖:ok 态渲染版本号 / error 态上浮 i18n 标签+消息(禁止吞错)/ loading 占位。
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { Toolbar, type IpcState } from "./Toolbar";

afterEach(cleanup);

describe("Toolbar IPC 三态", () => {
  it("ok 态渲染应用版本号", () => {
    const ipc: IpcState = { kind: "ok", version: "0.1.0" };
    render(<Toolbar ipc={ipc} />);
    expect(screen.getByText("v0.1.0")).toBeTruthy();
  });

  it("error 态上浮 i18n 标签与消息(不吞错)", () => {
    const ipc: IpcState = { kind: "error", message: "engine down" };
    render(<Toolbar ipc={ipc} />);
    expect(screen.getByText("IPC unavailable: engine down")).toBeTruthy();
  });

  it("loading 态渲染占位符", () => {
    const ipc: IpcState = { kind: "loading" };
    render(<Toolbar ipc={ipc} />);
    expect(screen.getByText("…")).toBeTruthy();
  });
});
