import { AlertCircle, X } from "lucide-react";

interface CreateSpaceModalProps {
  open: boolean;
  name: string;
  slug: string;
  error: string;
  busy: boolean;
  onChangeName: (name: string) => void;
  onChangeSlug: (slug: string) => void;
  onClose: () => void;
  onSubmit: () => void;
}

export function CreateSpaceModal({
  open,
  name,
  slug,
  error,
  busy,
  onChangeName,
  onChangeSlug,
  onClose,
  onSubmit
}: CreateSpaceModalProps) {
  if (!open) return null;

  const trimmedName = name.trim();
  const trimmedSlug = slug.trim();

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
              placeholder="例如：我的工作文档"
              onChange={(event) => onChangeName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && trimmedName && !busy) onSubmit();
                if (event.key === "Escape") onClose();
              }}
            />
          </label>
          <label>
            <span>远端仓库标识 (Slug)</span>
            <input
              value={slug}
              placeholder="例如：my_workspace"
              onChange={(event) => onChangeSlug(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && trimmedName && !busy) onSubmit();
                if (event.key === "Escape") onClose();
              }}
            />
            <span className="fieldHint">
              远端 ModelScope 数据集仓库名称，需使用小写字母、数字、短横线或下划线
            </span>
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
            disabled={!trimmedName || !trimmedSlug || busy}
          >
            创建
          </button>
        </div>
      </section>
    </div>
  );
}
