import { AlertTriangle, CheckCircle2, Info, X, XCircle } from "lucide-react";
import { useEffect, useRef, useState } from "react";

export type ToastType = "success" | "warning" | "error" | "info";

export type ToastItem = {
  id: string;
  type: ToastType;
  message: string;
  duration?: number;
};

const toastIcons = {
  success: CheckCircle2,
  warning: AlertTriangle,
  error: XCircle,
  info: Info
};

export function ToastMessageItem({
  toast,
  onDismiss
}: {
  toast: ToastItem;
  onDismiss: (id: string) => void;
}) {
  const { id, type, message, duration = 3500 } = toast;
  const Icon = toastIcons[type] || Info;

  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const remainingTimeRef = useRef(duration);
  const startTimeRef = useRef<number>(Date.now());
  const [isPaused, setIsPaused] = useState(false);

  useEffect(() => {
    if (duration <= 0 || isPaused) return;

    startTimeRef.current = Date.now();
    timerRef.current = setTimeout(() => {
      onDismiss(id);
    }, remainingTimeRef.current);

    return () => {
      if (timerRef.current) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
  }, [id, onDismiss, duration, isPaused]);

  const handleMouseEnter = () => {
    if (duration <= 0) return;
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    const elapsed = Date.now() - startTimeRef.current;
    remainingTimeRef.current = Math.max(0, remainingTimeRef.current - elapsed);
    setIsPaused(true);
  };

  const handleMouseLeave = () => {
    if (duration <= 0) return;
    setIsPaused(false);
  };

  return (
    <div
      role="alert"
      className={`toastItem toast-${type}`}
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
    >
      <Icon className="toastIcon" aria-hidden />
      <span className="toastText">{message}</span>
      <button
        type="button"
        className="toastCloseBtn"
        onClick={() => onDismiss(id)}
        title="关闭"
        aria-label="关闭"
      >
        <X aria-hidden />
      </button>
    </div>
  );
}

export function ToastContainer({
  toasts,
  onDismiss
}: {
  toasts: ToastItem[];
  onDismiss: (id: string) => void;
}) {
  if (toasts.length === 0) return null;

  return (
    <div className="toastContainer" aria-live="polite">
      {toasts.map((toast) => (
        <ToastMessageItem key={toast.id} toast={toast} onDismiss={onDismiss} />
      ))}
    </div>
  );
}
