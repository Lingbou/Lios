import { useCallback, useState } from "react";
import type { ToastItem, ToastType } from "./Toast.tsx";

export function useToasts() {
  const [toasts, setToasts] = useState<ToastItem[]>([]);

  const dismissToast = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  const showToast = useCallback((msg: string, type: ToastType = "info", duration = 3500) => {
    if (!msg) return;
    const id = `${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
    setToasts((prev) => [...prev.slice(-3), { id, type, message: msg, duration }]);
  }, []);

  const setMessage = useCallback((text: string, typeOverride?: ToastType) => {
    if (!text) {
      setToasts([]);
      return;
    }
    let type: ToastType = typeOverride ?? "info";
    if (!typeOverride) {
      if (text.includes("已导出") || text.includes("已导入") || text.includes("成功") || text.includes("完成")) {
        type = "success";
      } else if (
        text.includes("失败") ||
        text.includes("错误") ||
        text.includes("异常") ||
        text.includes("拒绝") ||
        text.startsWith("CommandError")
      ) {
        type = "error";
      } else if (
        text.includes("警告") ||
        text.includes("跳过") ||
        text.includes("冲突") ||
        text.includes("取消") ||
        text.includes("中止")
      ) {
        type = "warning";
      }
    }
    showToast(text, type);
  }, [showToast]);

  return { toasts, dismissToast, showToast, setMessage };
}
