import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { DriveToolbar } from "../../src/features/drive/DriveToolbar.tsx";

describe("DriveToolbar", () => {
  it("enables upload and new folder buttons when canUpload is true", () => {
    const onUploadFiles = vi.fn();
    const onNewFolder = vi.fn();

    render(
      <DriveToolbar
        canUpload={true}
        selectedCount={0}
        totalCount={5}
        viewMode="table"
        busy={false}
        onUploadFiles={onUploadFiles}
        onUploadFolder={vi.fn()}
        onNewFolder={onNewFolder}
        onDownload={vi.fn()}
        onRename={vi.fn()}
        onDelete={vi.fn()}
        onRefresh={vi.fn()}
        onClearSelection={vi.fn()}
        onViewModeChange={vi.fn()}
      />
    );

    const uploadBtn = screen.getByText("上传文件").closest("button")!;
    expect(uploadBtn).not.toBeDisabled();
    fireEvent.click(uploadBtn);
    expect(onUploadFiles).toHaveBeenCalledOnce();

    const newFolderBtn = screen.getByText("新建文件夹").closest("button")!;
    expect(newFolderBtn).not.toBeDisabled();
    fireEvent.click(newFolderBtn);
    expect(onNewFolder).toHaveBeenCalledOnce();
  });

  it("handles selection count and view mode toggle", () => {
    const onClearSelection = vi.fn();
    const onViewModeChange = vi.fn();

    render(
      <DriveToolbar
        canUpload={true}
        selectedCount={2}
        totalCount={10}
        viewMode="table"
        busy={false}
        onUploadFiles={vi.fn()}
        onUploadFolder={vi.fn()}
        onNewFolder={vi.fn()}
        onDownload={vi.fn()}
        onRename={vi.fn()}
        onDelete={vi.fn()}
        onRefresh={vi.fn()}
        onClearSelection={onClearSelection}
        onViewModeChange={onViewModeChange}
      />
    );

    expect(screen.getByText("已选 2 / 10 项")).toBeInTheDocument();
    const toolbar = screen.getByText("已选 2 / 10 项").closest(".driveToolbar")!;
    expect(toolbar.querySelector(":scope > .viewModeSegment")).not.toBeNull();
    expect(toolbar.querySelector(".toolbarActions .selectionBadge")).not.toBeNull();
    fireEvent.click(screen.getByText("取消"));
    expect(onClearSelection).toHaveBeenCalledOnce();

    const gridBtn = screen.getByTitle("网格平铺视图");
    fireEvent.click(gridBtn);
    expect(onViewModeChange).toHaveBeenCalledWith("grid");
  });
});
