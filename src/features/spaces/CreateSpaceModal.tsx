import { AlertCircle, X } from "lucide-react";

export interface CreateSpaceModalProps {
  open: boolean;
  name: string;
  repoId: string;
  error: string;
  busy: boolean;
  onChangeName: (name: string) => void;
  onChangeRepoId: (repoId: string) => void;
  onClose: () => void;
  onSubmit: () => void;
}

export function CreateSpaceModal({
  open,
  name,
  repoId,
  error,
  busy,
  onChangeName,
  onChangeRepoId,
  onClose,
  onSubmit
}: CreateSpaceModalProps) {
  if (!open) return null;

  const trimmedName = name.trim();
  const trimmedRepoId = repoId.trim();
  const isInvalidRepoId = Boolean(trimmedRepoId) && !/^[a-z][a-z0-9_-]{0,31}$/.test(trimmedRepoId);

  const displayError =
    error ||
    (!trimmedName
      ? ""
      : !trimmedRepoId
        ? "请输入远端仓库标识 (ID)"
        : isInvalidRepoId
          ? "仓库标识必须以小写英文开头，仅限小写英文、数字、_ 或 -，最长 32 字符"
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
            <span>空间名称 / 备注 (支持中文)</span>
            <input
              autoFocus
              value={name}
              placeholder="例如：工作文档、照片备份、photos"
              onChange={(event) => onChangeName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && trimmedName && trimmedRepoId && !isInvalidRepoId && !busy) onSubmit();
                if (event.key === "Escape") onClose();
              }}
            />
          </label>
          <label>
            <span>远端仓库 ID (ModelScope 英文标识)</span>
            <input
              value={repoId}
              placeholder="例如：work_docs (仅限小写英文、数字、-、_)"
              onChange={(event) => onChangeRepoId(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && trimmedName && trimmedRepoId && !isInvalidRepoId && !busy) onSubmit();
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
            disabled={!trimmedName || !trimmedRepoId || Boolean(displayError) || busy}
          >
            创建
          </button>
        </div>
      </section>
    </div>
  );
}
