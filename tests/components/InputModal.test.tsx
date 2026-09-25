import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { InputModal } from "../../src/components/InputModal.tsx";

describe("InputModal", () => {
  it("does not render when open is false", () => {
    const { container } = render(
      <InputModal
        open={false}
        title="测试标题"
        onClose={vi.fn()}
        onSubmit={vi.fn()}
      />
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders with title, description, and default value", () => {
    render(
      <InputModal
        open={true}
        title="新建文件夹"
        description="在当前目录下创建"
        inputLabel="文件夹名称"
        placeholder="请输入名称"
        defaultValue="新目录"
        onClose={vi.fn()}
        onSubmit={vi.fn()}
      />
    );

    expect(screen.getByText("新建文件夹")).toBeInTheDocument();
    expect(screen.getByText("在当前目录下创建")).toBeInTheDocument();
    expect(screen.getByText("文件夹名称")).toBeInTheDocument();
    const input = screen.getByPlaceholderText("请输入名称") as HTMLInputElement;
    expect(input.value).toBe("新目录");
  });

  it("validates input and disables submit on validation error", () => {
    const onSubmit = vi.fn();
    render(
      <InputModal
        open={true}
        title="重命名"
        defaultValue="file.txt"
        validate={(val) => (val.includes("/") ? "名称不能包含 /" : null)}
        onClose={vi.fn()}
        onSubmit={onSubmit}
      />
    );

    const input = screen.getByRole("textbox");
    fireEvent.change(input, { target: { value: "invalid/name" } });

    expect(screen.getByText("名称不能包含 /")).toBeInTheDocument();
    const confirmBtn = screen.getByText("确定");
    expect(confirmBtn).toBeDisabled();

    fireEvent.click(confirmBtn);
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("submits trimmed value on confirm click and Enter key", () => {
    const onSubmit = vi.fn();
    render(
      <InputModal
        open={true}
        title="重命名"
        defaultValue="old_name"
        onClose={vi.fn()}
        onSubmit={onSubmit}
      />
    );

    const input = screen.getByRole("textbox");
    fireEvent.change(input, { target: { value: "  new_name  " } });

    const confirmBtn = screen.getByText("确定");
    expect(confirmBtn).not.toBeDisabled();
    fireEvent.click(confirmBtn);
    expect(onSubmit).toHaveBeenCalledWith("new_name");

    fireEvent.keyDown(input, { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledWith("new_name");
  });

  it("calls onClose on cancel button, close button, or Escape key", () => {
    const onClose = vi.fn();
    render(
      <InputModal
        open={true}
        title="新建文件夹"
        onClose={onClose}
        onSubmit={vi.fn()}
      />
    );

    fireEvent.click(screen.getByText("取消"));
    expect(onClose).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByTitle("关闭"));
    expect(onClose).toHaveBeenCalledTimes(2);

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(3);
  });
});
