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
  const hasChinese = /[\u4e00-\u9fa5]/.test(trimmed);
  const isInvalidFormat = Boolean(trimmed) && !/^[a-z][a-z0-9_-]{0,31}$/.test(trimmed);

  const displayError =
    error ||
    (hasChinese
      ? "空间名称不支持中文，由于 ModelScope 底层 Git 兼容性限制，必须使用纯小写英文标识"
      : isInvalidFormat
        ? "必须以小写英文字母开头，仅限小写英文、数字、_ 或 -，最长 32 字符"
        : "");

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
            <span>空间名称 (英文标识)</span>
            <input
              autoFocus
              value={name}
              placeholder="例如 photos (仅限小写英文、数字、-、_)"
              onChange={(event) => onChangeName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && trimmed && !isInvalidFormat && !busy) onSubmit();
                if (event.key === "Escape") onClose();
              }}
            />
          </label>
          {displayError && (
            <div className="fieldError">
              <AlertCircle className="fieldErrorIcon" aria-hidden />
              <span>{displayError}</span>
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
            disabled={!trimmed || Boolean(displayError) || busy}
          >
            创建
          </button>
        </div>
      </section>
    </div>
  );
}
