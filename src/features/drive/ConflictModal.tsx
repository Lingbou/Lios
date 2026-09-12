import { X } from "lucide-react";
import type { ConflictAction, UploadConflict } from "../../appTypes.ts";

export interface ConflictModalProps {
  conflicts: UploadConflict[];
  conflictActions: Record<string, ConflictAction>;
  onSetAction: (sourcePath: string, action: ConflictAction) => void;
  onSetAllActions: (action: ConflictAction) => void;
  onCancel: () => void;
  onConfirm: () => void;
}

export function ConflictModal({
  conflicts,
  conflictActions,
  onSetAction,
  onSetAllActions,
  onCancel,
  onConfirm
}: ConflictModalProps) {
  if (conflicts.length === 0) return null;

  return (
    <div className="modalBackdrop">
      <section
        className="conflictModal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="conflict-modal-title"
      >
        <div className="modalHeader">
          <div>
            <h2 id="conflict-modal-title">处理同名项目</h2>
            <p>发现 {conflicts.length} 个同名项目，请选择处理方式</p>
          </div>
          <button onClick={onCancel} title="关闭" aria-label="关闭">
            <X aria-hidden />
          </button>
        </div>

        <div className="conflictBulkBar">
          <span>批量设置：</span>
          <div className="bulkButtons">
            <button
              type="button"
              className="bulkActionBtn"
              onClick={() => onSetAllActions("KeepBoth")}
            >
              全部保留两者
            </button>
            <button
              type="button"
              className="bulkActionBtn"
              onClick={() => onSetAllActions("Replace")}
            >
              全部替换
            </button>
            <button
              type="button"
              className="bulkActionBtn"
              onClick={() => onSetAllActions("Skip")}
            >
              全部跳过
            </button>
          </div>
        </div>

        <div className="conflictList">
          {conflicts.map((conflict) => (
            <label className="conflictItem" key={conflict.source_path}>
              <span>
                <strong>{conflict.target_name}</strong>
                <small title={conflict.source_path}>{conflict.source_path}</small>
              </span>
              <select
                value={conflictActions[conflict.source_path] || "KeepBoth"}
                onChange={(event) =>
                  onSetAction(conflict.source_path, event.target.value as ConflictAction)
                }
              >
                <option value="KeepBoth">保留两者</option>
                <option value="Replace">替换</option>
                <option value="Skip">跳过</option>
              </select>
            </label>
          ))}
        </div>

        <div className="modalActions">
          <button type="button" onClick={onCancel}>
            取消
          </button>
          <button type="button" className="primary" onClick={onConfirm}>
            继续上传
          </button>
        </div>
      </section>
    </div>
  );
}
