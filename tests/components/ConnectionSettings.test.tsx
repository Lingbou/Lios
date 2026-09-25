import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
  ConnectionSection,
  EndpointSection
} from "../../src/features/settings/ConnectionSettings.tsx";

describe("ConnectionSection", () => {
  it("renders connect prompt when unauthenticated and connects when token is entered", () => {
    const onTokenChange = vi.fn();
    const onConnect = vi.fn();
    const onDisconnect = vi.fn();

    const { rerender } = render(
      <ConnectionSection
        hasToken={false}
        username={null}
        token=""
        onTokenChange={onTokenChange}
        onConnect={onConnect}
        onDisconnect={onDisconnect}
        busy={false}
      />
    );

    expect(screen.getByRole("heading", { name: "连接" })).toBeInTheDocument();
    expect(
      screen.getByText("输入访问凭证连接 ModelScope 账号")
    ).toBeInTheDocument();
    expect(screen.queryByText("断开连接")).not.toBeInTheDocument();

    const connectButton = screen.getByRole("button", { name: "连接" });
    expect(connectButton).toBeDisabled();

    const input = screen.getByPlaceholderText("ModelScope access token");
    fireEvent.change(input, { target: { value: "my-test-token" } });
    expect(onTokenChange).toHaveBeenCalledWith("my-test-token");

    rerender(
      <ConnectionSection
        hasToken={false}
        username={null}
        token="my-test-token"
        onTokenChange={onTokenChange}
        onConnect={onConnect}
        onDisconnect={onDisconnect}
        busy={false}
      />
    );

    const updatedButton = screen.getByRole("button", { name: "连接" });
    expect(updatedButton).toBeEnabled();
    fireEvent.click(updatedButton);
    expect(onConnect).toHaveBeenCalledOnce();
  });

  it("renders authenticated state with username and disconnect button", () => {
    const onTokenChange = vi.fn();
    const onConnect = vi.fn();
    const onDisconnect = vi.fn();

    render(
      <ConnectionSection
        hasToken={true}
        username="alice"
        token=""
        onTokenChange={onTokenChange}
        onConnect={onConnect}
        onDisconnect={onDisconnect}
        busy={false}
      />
    );

    expect(screen.getByText("已连接 alice")).toBeInTheDocument();
    const disconnectButton = screen.getByRole("button", { name: "断开连接" });
    expect(disconnectButton).toBeEnabled();
    fireEvent.click(disconnectButton);
    expect(onDisconnect).toHaveBeenCalledOnce();

    const updateButton = screen.getByRole("button", { name: "更新凭据" });
    expect(updateButton).toBeDisabled();
    expect(
      screen.getByPlaceholderText("输入新的 Access Token 以更新凭据")
    ).toBeInTheDocument();
  });

  it("disables actions while busy", () => {
    const onTokenChange = vi.fn();
    const onConnect = vi.fn();
    const onDisconnect = vi.fn();

    render(
      <ConnectionSection
        hasToken={true}
        username="alice"
        token="new-token"
        onTokenChange={onTokenChange}
        onConnect={onConnect}
        onDisconnect={onDisconnect}
        busy={true}
      />
    );

    expect(screen.getByRole("button", { name: "断开连接" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "更新凭据" })).toBeDisabled();
  });
});

describe("EndpointSection", () => {
  it("allows independently configuring and saving endpoint without token", () => {
    const onEndpointChange = vi.fn();
    const onSave = vi.fn();

    const { rerender } = render(
      <EndpointSection
        endpoint="https://modelscope.cn"
        onEndpointChange={onEndpointChange}
        onSave={onSave}
        busy={false}
      />
    );

    expect(screen.getByRole("heading", { name: "服务地址" })).toBeInTheDocument();
    expect(
      screen.getByText("ModelScope 平台服务与 API 端点")
    ).toBeInTheDocument();

    const input = screen.getByDisplayValue("https://modelscope.cn");
    const saveButton = screen.getByRole("button", { name: "保存端点" });
    expect(saveButton).toBeEnabled();

    fireEvent.change(input, {
      target: { value: "https://www.modelscope.cn" }
    });
    expect(onEndpointChange).toHaveBeenCalledWith("https://www.modelscope.cn");

    fireEvent.click(saveButton);
    expect(onSave).toHaveBeenCalledOnce();

    // Disabled when empty
    rerender(
      <EndpointSection
        endpoint="   "
        onEndpointChange={onEndpointChange}
        onSave={onSave}
        busy={false}
      />
    );
    expect(screen.getByRole("button", { name: "保存端点" })).toBeDisabled();

    // Disabled when busy
    rerender(
      <EndpointSection
        endpoint="https://modelscope.cn"
        onEndpointChange={onEndpointChange}
        onSave={onSave}
        busy={true}
      />
    );
    expect(screen.getByRole("button", { name: "保存端点" })).toBeDisabled();
  });
});
