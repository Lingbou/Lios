import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ConfirmModal } from "../../src/components/ConfirmModal.tsx";

describe("ConfirmModal", () => {
  it("does not render when open is false", () => {
    const { container } = render(
      <ConfirmModal
        open={false}
        title="确认删除"
        onClose={vi.fn()}
        onConfirm={vi.fn()}
      />
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders title, description, and notice details", () => {
    render(
      <ConfirmModal
        open={true}
        title="删除项目"
        description="确定删除选中的 3 个项目？"
        details="远端文件不受影响。"
        confirmText="删除"
        danger
        onClose={vi.fn()}
        onConfirm={vi.fn()}
      />
    );

    expect(screen.getByText("删除项目")).toBeInTheDocument();
    expect(screen.getByText("确定删除选中的 3 个项目？")).toBeInTheDocument();
    expect(screen.getByText("远端文件不受影响。")).toBeInTheDocument();
    const confirmBtn = screen.getByText("删除");
    expect(confirmBtn).toHaveClass("danger");
  });

  it("supports alert mode by hiding cancel button", () => {
    render(
      <ConfirmModal
        open={true}
        title="暂不支持同步"
        description="含中文字符的数据集暂不支持"
        confirmText="知道了"
        hideCancel
        onClose={vi.fn()}
        onConfirm={vi.fn()}
      />
    );

    expect(screen.getByText("暂不支持同步")).toBeInTheDocument();
    expect(screen.queryByText("取消")).not.toBeInTheDocument();
    expect(screen.getByText("知道了")).toBeInTheDocument();
  });

  it("handles confirm and cancel actions", () => {
    const onConfirm = vi.fn();
    const onClose = vi.fn();

    render(
      <ConfirmModal
        open={true}
        title="确认操作"
        onClose={onClose}
        onConfirm={onConfirm}
      />
    );

    fireEvent.click(screen.getByText("确定"));
    expect(onConfirm).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByText("取消"));
    expect(onClose).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(window, { key: "Enter" });
    expect(onConfirm).toHaveBeenCalledTimes(2);

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});
