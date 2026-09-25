import { AlertCircle, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";

export interface InputModalProps {
  open: boolean;
  title: string;
  description?: string;
  inputLabel?: string;
  placeholder?: string;
  defaultValue?: string;
  confirmText?: string;
  cancelText?: string;
  busy?: boolean;
  validate?: (value: string) => string | null | undefined;
  onClose: () => void;
  onSubmit: (value: string) => void | Promise<void>;
}

export function InputModal({
  open,
  title,
  description,
  inputLabel,
  placeholder,
  defaultValue = "",
  confirmText = "确定",
  cancelText = "取消",
  busy = false,
  validate,
  onClose,
  onSubmit
}: InputModalProps) {
  const [value, setValue] = useState(defaultValue);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) {
      setValue(defaultValue);
      setError(null);
      const timer = setTimeout(() => {
        inputRef.current?.focus();
        inputRef.current?.select();
      }, 0);
      return () => clearTimeout(timer);
    }
  }, [open, defaultValue]);

  useEffect(() => {
    if (!open) return;
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      }
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  const handleSubmit = () => {
    if (busy) return;
    const trimmed = value.trim();
    if (!trimmed) {
      setError("输入不能为空");
      return;
    }
    const err = validate?.(trimmed);
    if (err) {
      setError(err);
      return;
    }
    void onSubmit(trimmed);
  };

  return (
    <div className="modalBackdrop" onClick={onClose}>
      <section
        className="inputModal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="input-modal-title"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modalHeader">
          <div>
            <h2 id="input-modal-title">{title}</h2>
            {description && <p>{description}</p>}
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

        <div className="inputModalBody">
          <label>
            {inputLabel && <span>{inputLabel}</span>}
            <input
              ref={inputRef}
              type="text"
              value={value}
              placeholder={placeholder}
              disabled={busy}
              autoFocus
              onChange={(event) => {
                const next = event.target.value;
                setValue(next);
                if (validate) {
                  setError(validate(next) ?? null);
                } else {
                  setError(null);
                }
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  handleSubmit();
                }
              }}
            />
          </label>
          {error && (
            <div className="fieldError" role="alert">
              <AlertCircle className="fieldErrorIcon" aria-hidden />
              <span>{error}</span>
            </div>
          )}
        </div>

        <div className="modalActions">
          <button type="button" onClick={onClose} disabled={busy}>
            {cancelText}
          </button>
          <button
            type="button"
            className="primary"
            onClick={handleSubmit}
            disabled={!value.trim() || !!error || busy}
          >
            {confirmText}
          </button>
        </div>
      </section>
    </div>
  );
}
