import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { DriveItem } from "../../src/appTypes.ts";
import { FileTable } from "../../src/features/drive/FileTable.tsx";

const mockItems: DriveItem[] = [
  {
    id: "dir-1",
    name: "Documents",
    kind: "Directory",
    size: 0,
    updated_at: "2026-08-01T10:00:00Z",
    children_count: 3
  },
  {
    id: "file-1",
    name: "report.pdf",
    kind: "File",
    size: 1048576, // 1 MB
    updated_at: "2026-08-02T12:00:00Z",
    children_count: 0
  }
];

describe("FileTable", () => {
  it("renders items with correct metadata and names", () => {
    render(
      <FileTable
        items={mockItems}
        selectedIds={new Set()}
        sortField="name"
        sortDirection="asc"
        onSort={vi.fn()}
        onToggleSelect={vi.fn()}
        onSelectOnly={vi.fn()}
        onSelectRange={vi.fn()}
        onSelectAll={vi.fn()}
        onEnterItem={vi.fn()}
        onContextMenu={vi.fn()}
      />
    );

    expect(screen.getByText("Documents")).toBeInTheDocument();
    expect(screen.getByText("report.pdf")).toBeInTheDocument();
    expect(screen.getByText("3 项")).toBeInTheDocument();
    expect(screen.getByText("1.00 MB")).toBeInTheDocument();
  });

  it("handles header sorting clicks", () => {
    const onSort = vi.fn();
    render(
      <FileTable
        items={mockItems}
        selectedIds={new Set()}
        sortField="name"
        sortDirection="asc"
        onSort={onSort}
        onToggleSelect={vi.fn()}
        onSelectOnly={vi.fn()}
        onSelectRange={vi.fn()}
        onSelectAll={vi.fn()}
        onEnterItem={vi.fn()}
        onContextMenu={vi.fn()}
      />
    );

    fireEvent.click(screen.getByText("大小"));
    expect(onSort).toHaveBeenCalledWith("size");

    fireEvent.click(screen.getByText("修改时间"));
    expect(onSort).toHaveBeenCalledWith("updated_at");
  });

  it("handles row clicks and double-clicks", () => {
    const onSelectOnly = vi.fn();
    const onEnterItem = vi.fn();

    render(
      <FileTable
        items={mockItems}
        selectedIds={new Set()}
        sortField="name"
        sortDirection="asc"
        onSort={vi.fn()}
        onToggleSelect={vi.fn()}
        onSelectOnly={onSelectOnly}
        onSelectRange={vi.fn()}
        onSelectAll={vi.fn()}
        onEnterItem={onEnterItem}
        onContextMenu={vi.fn()}
      />
    );

    const docRow = screen.getByText("Documents").closest("tr")!;
    fireEvent.click(docRow);
    expect(onSelectOnly).toHaveBeenCalledWith("dir-1");

    fireEvent.doubleClick(docRow);
    expect(onEnterItem).toHaveBeenCalledWith(mockItems[0]);

    // Clicking directly on the filename element selects the item (does not enter)
    const fileSpan = screen.getByText("report.pdf");
    fireEvent.click(fileSpan);
    expect(onSelectOnly).toHaveBeenCalledWith("file-1");

    // Double clicking directly on the filename enters the item
    fireEvent.doubleClick(fileSpan);
    expect(onEnterItem).toHaveBeenCalledWith(mockItems[1]);
  });

  it("handles select all in header", () => {
    const onSelectAll = vi.fn();

    render(
      <FileTable
        items={mockItems}
        selectedIds={new Set(["dir-1"])}
        sortField="name"
        sortDirection="asc"
        onSort={vi.fn()}
        onToggleSelect={vi.fn()}
        onSelectOnly={vi.fn()}
        onSelectRange={vi.fn()}
        onSelectAll={onSelectAll}
        onEnterItem={vi.fn()}
        onContextMenu={vi.fn()}
      />
    );

    const headerCheckbox = screen.getByTitle("全选 (Ctrl+A)");
    fireEvent.click(headerCheckbox);
    expect(onSelectAll).toHaveBeenCalledOnce();
  });
});
