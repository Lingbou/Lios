import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { DriveItem } from "../../src/appTypes.ts";
import { ContextMenu } from "../../src/features/drive/ContextMenu.tsx";

const mockFile: DriveItem = {
  id: "file-1",
  name: "photo.png",
  kind: "File",
  size: 50000,
  updated_at: "2026-08-01T10:00:00Z",
  children_count: 0
};

describe("ContextMenu", () => {
  it("does not render when state.open is false", () => {
    const { container } = render(
      <ContextMenu
        state={{ open: false, x: 0, y: 0, item: null }}
        onClose={vi.fn()}
        selectedCount={0}
      />
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders file actions when opened on a file item", () => {
    const onDownload = vi.fn();
    const onRename = vi.fn();
    const onDelete = vi.fn();
    const onClose = vi.fn();

    render(
      <ContextMenu
        state={{ open: true, x: 100, y: 150, item: mockFile }}
        onClose={onClose}
        selectedCount={1}
        onDownload={onDownload}
        onRename={onRename}
        onDelete={onDelete}
      />
    );

    expect(screen.getByText("photo.png")).toBeInTheDocument();
    expect(screen.getByText("下载")).toBeInTheDocument();
    expect(screen.getByText("重命名")).toBeInTheDocument();
    expect(screen.getByText("删除")).toBeInTheDocument();

    fireEvent.click(screen.getByText("下载"));
    expect(onDownload).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("renders canvas actions when opened on empty area", () => {
    const onUploadFiles = vi.fn();
    const onNewFolder = vi.fn();
    const onSelectAll = vi.fn();
    const onClose = vi.fn();

    render(
      <ContextMenu
        state={{ open: true, x: 200, y: 250, item: null }}
        onClose={onClose}
        selectedCount={0}
        onUploadFiles={onUploadFiles}
        onNewFolder={onNewFolder}
        onSelectAll={onSelectAll}
      />
    );

    expect(screen.getByText("上传文件")).toBeInTheDocument();
    expect(screen.getByText("新建文件夹")).toBeInTheDocument();
    expect(screen.getByText("全选")).toBeInTheDocument();

    fireEvent.click(screen.getByText("上传文件"));
    expect(onUploadFiles).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
  });
});
