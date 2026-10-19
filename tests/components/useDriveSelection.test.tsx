import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { useDriveSelection } from "../../src/features/drive/useDriveSelection.ts";
import type { DriveItem } from "../../src/appTypes.ts";

const mockItems: DriveItem[] = [
  { id: "1", name: "file1.txt", kind: "File", size: 100, updated_at: "2026-01-01" },
  { id: "2", name: "file2.txt", kind: "File", size: 200, updated_at: "2026-01-02" },
  { id: "3", name: "folder1", kind: "Directory", size: 0, updated_at: "2026-01-03" }
];

describe("useDriveSelection", () => {
  it("selects only one item on selectOnly", () => {
    const { result } = renderHook(() => useDriveSelection(mockItems));
    act(() => {
      result.current.selectOnly("1");
    });
    expect(result.current.selectedIds).toEqual(new Set(["1"]));
    expect(result.current.lastSelectedId).toBe("1");
  });

  it("toggles item selection", () => {
    const { result } = renderHook(() => useDriveSelection(mockItems));
    act(() => {
      result.current.toggleSelection("1");
    });
    expect(result.current.selectedIds.has("1")).toBe(true);

    act(() => {
      result.current.toggleSelection("1");
    });
    expect(result.current.selectedIds.has("1")).toBe(false);
  });

  it("selects a continuous range of items", () => {
    const { result } = renderHook(() => useDriveSelection(mockItems));
    act(() => {
      result.current.selectOnly("1");
    });
    act(() => {
      result.current.selectRange("3");
    });
    expect(result.current.selectedIds).toEqual(new Set(["1", "2", "3"]));
  });

  it("toggles select all", () => {
    const { result } = renderHook(() => useDriveSelection(mockItems));
    act(() => {
      result.current.selectAll();
    });
    expect(result.current.selectedIds.size).toBe(3);

    act(() => {
      result.current.selectAll();
    });
    expect(result.current.selectedIds.size).toBe(0);
  });

  it("clears selection cleanly", () => {
    const { result } = renderHook(() => useDriveSelection(mockItems));
    act(() => {
      result.current.selectOnly("2");
    });
    act(() => {
      result.current.clearSelection();
    });
    expect(result.current.selectedIds.size).toBe(0);
    expect(result.current.lastSelectedId).toBeNull();
  });
});