import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useWindowControls } from "../../src/hooks/useWindowControls.ts";

const mockCurrentWindow = {
  minimize: vi.fn().mockResolvedValue(undefined),
  toggleMaximize: vi.fn().mockResolvedValue(undefined),
  close: vi.fn().mockResolvedValue(undefined),
  startDragging: vi.fn().mockResolvedValue(undefined)
};

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => mockCurrentWindow
}));

describe("useWindowControls", () => {
  it("calls window APIs when tauri runtime is simulated", async () => {
    (window as any).__TAURI_INTERNALS__ = {};
    const onError = vi.fn();
    const { result } = renderHook(() => useWindowControls(onError));

    await act(async () => {
      await result.current.minimizeWindow();
    });
    expect(mockCurrentWindow.minimize).toHaveBeenCalledOnce();

    await act(async () => {
      await result.current.toggleMaximizeWindow();
    });
    expect(mockCurrentWindow.toggleMaximize).toHaveBeenCalledOnce();

    await act(async () => {
      await result.current.closeWindow();
    });
    expect(mockCurrentWindow.close).toHaveBeenCalledOnce();

    delete (window as any).__TAURI_INTERNALS__;
  });
});