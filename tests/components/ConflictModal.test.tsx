import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { UploadConflict } from "../../src/appTypes.ts";
import { ConflictModal } from "../../src/features/drive/ConflictModal.tsx";

const mockConflicts: UploadConflict[] = [
  {
    source_path: "/home/user/doc.txt",
    target_name: "doc.txt",
    existing_node_id: "node-1",
    kind: "File"
  },
  {
    source_path: "/home/user/pic.png",
    target_name: "pic.png",
    existing_node_id: "node-2",
    kind: "File"
  }
];

describe("ConflictModal", () => {
  it("renders conflict items and supports bulk actions", () => {
    const onSetAction = vi.fn();
    const onSetAllActions = vi.fn();
    const onConfirm = vi.fn();
    const onCancel = vi.fn();

    render(
      <ConflictModal
        conflicts={mockConflicts}
        conflictActions={{}}
        onSetAction={onSetAction}
        onSetAllActions={onSetAllActions}
        onCancel={onCancel}
        onConfirm={onConfirm}
      />
    );

    expect(screen.getByText("处理同名项目")).toBeInTheDocument();
    expect(screen.getByText("doc.txt")).toBeInTheDocument();
    expect(screen.getByText("pic.png")).toBeInTheDocument();

    fireEvent.click(screen.getByText("全部替换"));
    expect(onSetAllActions).toHaveBeenCalledWith("Replace");

    fireEvent.click(screen.getByText("全部跳过"));
    expect(onSetAllActions).toHaveBeenCalledWith("Skip");

    fireEvent.click(screen.getByText("继续上传"));
    expect(onConfirm).toHaveBeenCalledOnce();
  });
});
