import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { DriveItem } from "../../src/appTypes.ts";
import { FilePreviewModal } from "../../src/features/drive/FilePreviewModal.tsx";

const mockFile: DriveItem = {
  id: "file-doc",
  name: "notes.md",
  kind: "File",
  size: 2048,
  updated_at: "2026-08-01T10:00:00Z",
  children_count: 0
};

describe("FilePreviewModal", () => {
  it("does not render when open is false", () => {
    const { container } = render(
      <FilePreviewModal
        open={false}
        item={mockFile}
        loading={false}
        error={null}
        content={null}
        onClose={vi.fn()}
        onDownload={vi.fn()}
      />
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders loading state", () => {
    render(
      <FilePreviewModal
        open={true}
        item={mockFile}
        loading={true}
        error={null}
        content={null}
        onClose={vi.fn()}
        onDownload={vi.fn()}
      />
    );

    expect(screen.getByText("正在解密并加载预览...")).toBeInTheDocument();
  });

  it("renders text content when isText is true", () => {
    render(
      <FilePreviewModal
        open={true}
        item={mockFile}
        loading={false}
        error={null}
        content={{ isText: true, text: "# Hello Lios" }}
        onClose={vi.fn()}
        onDownload={vi.fn()}
      />
    );

    expect(screen.getByText("notes.md")).toBeInTheDocument();
    expect(screen.getByText("# Hello Lios")).toBeInTheDocument();
  });

  it("renders image preview when dataUrl is present", () => {
    const mockImage: DriveItem = { ...mockFile, name: "photo.png" };
    render(
      <FilePreviewModal
        open={true}
        item={mockImage}
        loading={false}
        error={null}
        content={{ isText: false, dataUrl: "data:image/png;base64,iVBOR" }}
        onClose={vi.fn()}
        onDownload={vi.fn()}
      />
    );

    const img = screen.getByRole("img", { name: "photo.png" });
    expect(img).toHaveAttribute("src", "data:image/png;base64,iVBOR");
  });

  it("handles download and close callbacks", () => {
    const onDownload = vi.fn();
    const onClose = vi.fn();

    render(
      <FilePreviewModal
        open={true}
        item={mockFile}
        loading={false}
        error={null}
        content={{ isText: true, text: "content" }}
        onClose={onClose}
        onDownload={onDownload}
      />
    );

    fireEvent.click(screen.getByText("下载"));
    expect(onDownload).toHaveBeenCalledOnce();

    fireEvent.click(screen.getByTitle("关闭 (Esc)"));
    expect(onClose).toHaveBeenCalledOnce();
  });
});
