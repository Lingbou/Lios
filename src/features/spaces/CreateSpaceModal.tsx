import { AlertCircle, X } from "lucide-react";

export interface CreateSpaceModalProps {
  open: boolean;
  name: string;
  error: string;
  busy: boolean;
  onChangeName: (name: string) => void;
  onClose: () => void;
  onSubmit: () => void;
}

export function CreateSpaceModal({
  open,
  name,
  error,
  busy,
  onChangeName,
  onClose,
  onSubmit
}: CreateSpaceModalProps) {
  if (!open) return null;

  const trimmed = name.trim();

  return (
    <div className="modalBackdrop">
      <section
        className="spaceModal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="create-space-title"
      >
        <div className="modalHeader">
          <div>
            <h2 id="create-space-title">创建空间</h2>
            <p>在当前 ModelScope 账号下创建新的加密存储驱动器</p>
          </div>
          <button onClick={onClose} title="关闭" aria-label="关闭">
            <X aria-hidden />
          </button>
        </div>
        <div className="spaceModalBody">
          <label>
            <span>空间名称</span>
            <input
              autoFocus
              value={name}
              onChange={(event) => onChangeName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && trimmed && !busy) onSubmit();
                if (event.key === "Escape") onClose();
              }}
            />
          </label>
          {error && (
            <div className="fieldError">
              <AlertCircle className="fieldErrorIcon" aria-hidden />
              <span>{error}</span>
            </div>
          )}
        </div>
        <div className="modalActions">
          <button type="button" onClick={onClose}>
            取消
          </button>
          <button
            type="button"
            className="primary"
            onClick={onSubmit}
            disabled={!trimmed || busy}
          >
            创建
          </button>
        </div>
      </section>
    </div>
  );
}
