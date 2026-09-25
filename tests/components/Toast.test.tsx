import { fireEvent, render, screen, act } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ToastContainer, type ToastItem } from "../../src/features/toast/Toast.tsx";

describe("ToastContainer", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("renders toast items with proper messages and styles", () => {
    const toasts: ToastItem[] = [
      { id: "1", type: "success", message: "导出成功" },
      { id: "2", type: "error", message: "操作失败" },
      { id: "3", type: "warning", message: "注意空间配额" },
      { id: "4", type: "info", message: "新版本可用" }
    ];
    const onDismiss = vi.fn();

    render(<ToastContainer toasts={toasts} onDismiss={onDismiss} />);

    expect(screen.getByText("导出成功")).toBeInTheDocument();
    expect(screen.getByText("操作失败")).toBeInTheDocument();
    expect(screen.getByText("注意空间配额")).toBeInTheDocument();
    expect(screen.getByText("新版本可用")).toBeInTheDocument();

    const alerts = screen.getAllByRole("alert");
    expect(alerts).toHaveLength(4);
    expect(alerts[0]).toHaveClass("toast-success");
    expect(alerts[1]).toHaveClass("toast-error");
    expect(alerts[2]).toHaveClass("toast-warning");
    expect(alerts[3]).toHaveClass("toast-info");
  });

  it("calls onDismiss when close button is clicked", () => {
    const toasts: ToastItem[] = [{ id: "test-1", type: "info", message: "提示信息" }];
    const onDismiss = vi.fn();

    render(<ToastContainer toasts={toasts} onDismiss={onDismiss} />);

    const closeBtn = screen.getByRole("button", { name: "关闭" });
    fireEvent.click(closeBtn);

    expect(onDismiss).toHaveBeenCalledWith("test-1");
  });

  it("auto-dismisses after duration", () => {
    const toasts: ToastItem[] = [
      { id: "auto-1", type: "success", message: "即将消失", duration: 3000 }
    ];
    const onDismiss = vi.fn();

    render(<ToastContainer toasts={toasts} onDismiss={onDismiss} />);

    expect(onDismiss).not.toHaveBeenCalled();

    act(() => {
      vi.advanceTimersByTime(2999);
    });
    expect(onDismiss).not.toHaveBeenCalled();

    act(() => {
      vi.advanceTimersByTime(2);
    });
    expect(onDismiss).toHaveBeenCalledWith("auto-1");
  });

  it("pauses timer on hover and resumes on mouse leave", () => {
    const toasts: ToastItem[] = [
      { id: "pause-1", type: "warning", message: "悬停暂停", duration: 4000 }
    ];
    const onDismiss = vi.fn();

    render(<ToastContainer toasts={toasts} onDismiss={onDismiss} />);

    const toastElement = screen.getByRole("alert");

    // Advance 2 seconds
    act(() => {
      vi.advanceTimersByTime(2000);
    });

    // Hover to pause
    fireEvent.mouseEnter(toastElement);

    // Advance 3 more seconds while paused
    act(() => {
      vi.advanceTimersByTime(3000);
    });
    expect(onDismiss).not.toHaveBeenCalled();

    // Mouse leave to resume
    fireEvent.mouseLeave(toastElement);

    // Advance 1900 ms (total was 2000 remaining, 1900 is not enough)
    act(() => {
      vi.advanceTimersByTime(1900);
    });
    expect(onDismiss).not.toHaveBeenCalled();

    // Advance remaining 200 ms
    act(() => {
      vi.advanceTimersByTime(200);
    });
    expect(onDismiss).toHaveBeenCalledWith("pause-1");
  });
});
