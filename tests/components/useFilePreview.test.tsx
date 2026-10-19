import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useFilePreview } from "../../src/features/drive/useFilePreview.ts";
import type { DriveItem, SpaceSummary } from "../../src/appTypes.ts";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const mockSpace: SpaceSummary = {
  space_name: "test-space",
  endpoint: "https://modelscope.cn",
  namespace: "test-ns",
  dataset: "test-ds"
};

const mockItem: DriveItem = {
  id: "file-node-1",
  name: "doc.txt",
  kind: "File",
  size: 50,
  updated_at: "2026-01-01"
};

describe("useFilePreview", () => {
  it("initializes with closed preview state", () => {
    const { result } = renderHook(() => useFilePreview(mockSpace));
    expect(result.current.previewOpen).toBe(false);
    expect(result.current.previewItem).toBeNull();
    expect(result.current.previewLoading).toBe(false);
  });

  it("loads preview content on openFilePreview", async () => {
    const { invoke } = await import("@tauri-apps/api/core");
    vi.mocked(invoke).mockResolvedValueOnce({
      name: "doc.txt",
      size: 50,
      mime_type: "text/plain",
      is_text: true,
      text: "hello world"
    });

    const { result } = renderHook(() => useFilePreview(mockSpace));
    await act(async () => {
      await result.current.openFilePreview(mockItem);
    });

    expect(result.current.previewOpen).toBe(true);
    expect(result.current.previewItem).toEqual(mockItem);
    expect(result.current.previewContent?.text).toBe("hello world");
    expect(result.current.previewLoading).toBe(false);
  });

  it("closes preview cleanly on closePreview", async () => {
    const { result } = renderHook(() => useFilePreview(mockSpace));
    act(() => {
      result.current.closePreview();
    });
    expect(result.current.previewOpen).toBe(false);
    expect(result.current.previewItem).toBeNull();
  });
});