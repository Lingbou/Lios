import { AlertCircle, AlertTriangle, X } from "lucide-react";
import { useEffect, useRef } from "react";

export interface ConfirmModalProps {
  open: boolean;
  title: string;
  description?: string;
  details?: string | React.ReactNode;
  confirmText?: string;
  cancelText?: string;
  danger?: boolean;
  busy?: boolean;
  hideCancel?: boolean;
  onClose: () => void;
  onConfirm: () => void | Promise<void>;
}

export function ConfirmModal({
  open,
  title,
  description,
  details,
  confirmText = "确定",
  cancelText = "取消",
  danger = false,
  busy = false,
  hideCancel = false,
  onClose,
  onConfirm
}: ConfirmModalProps) {
  const confirmBtnRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (open) {
      const timer = setTimeout(() => {
        confirmBtnRef.current?.focus();
      }, 0);
      return () => clearTimeout(timer);
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      } else if (event.key === "Enter" && !busy) {
        event.preventDefault();
        void onConfirm();
      }
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [open, busy, onClose, onConfirm]);

  if (!open) return null;

  return (
    <div className="modalBackdrop" onClick={onClose}>
      <section
        className="confirmModal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="confirm-modal-title"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modalHeader">
          <div>
            <h2 id="confirm-modal-title">{title}</h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            title="关闭"
            aria-label="关闭"
            disabled={busy}
          >
            <X aria-hidden />
          </button>
        </div>

        <div className="confirmModalBody">
          {description && <p className="confirmModalDescription">{description}</p>}
          {details && (
            <div className={`confirmModalNotice ${danger ? "danger" : ""}`}>
              {danger ? (
                <AlertTriangle className="confirmNoticeIcon" aria-hidden />
              ) : (
                <AlertCircle className="confirmNoticeIcon" aria-hidden />
              )}
              <div className="confirmNoticeContent">
                {typeof details === "string" ? <p>{details}</p> : details}
              </div>
            </div>
          )}
        </div>

        <div className="modalActions">
          {!hideCancel && (
            <button type="button" onClick={onClose} disabled={busy}>
              {cancelText}
            </button>
          )}
          <button
            ref={confirmBtnRef}
            type="button"
            className={danger ? "danger" : "primary"}
            onClick={onConfirm}
            disabled={busy}
          >
            {confirmText}
          </button>
        </div>
      </section>
    </div>
  );
}
