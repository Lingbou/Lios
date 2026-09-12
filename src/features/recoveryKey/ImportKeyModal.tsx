import { AlertTriangle, CheckCircle2, KeyRound, X } from "lucide-react";
import type { RecoveryKeyImportDialog } from "../../appTypes.ts";
import {
  conciseRecoveryKeyPath,
  recoveryKeyConfirmationText
} from "../../recoveryKeyPresentation.ts";

export interface ImportKeyModalProps {
  dialog: RecoveryKeyImportDialog | null;
  onClose: () => void;
  onConfirm: () => void;
}

export function ImportKeyModal({ dialog, onClose, onConfirm }: ImportKeyModalProps) {
  if (!dialog) return null;

  return (
    <div className="modalBackdrop">
      <section
        className="recoveryKeyModal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="recovery-key-import-title"
      >
        <div className="modalHeader">
          <div>
            <h2 id="recovery-key-import-title">导入恢复密钥</h2>
            <p>{conciseRecoveryKeyPath(dialog.path)}</p>
          </div>
          <button
            onClick={onClose}
            disabled={dialog.importing}
            title="关闭"
            aria-label="关闭"
          >
            <X aria-hidden />
          </button>
        </div>
        <div className="recoveryKeyModalBody">
          <div className="recoveryVerificationResult">
            <CheckCircle2 aria-hidden />
            <span>
              <strong>密钥验证通过</strong>
              <small>{recoveryKeyConfirmationText(dialog.verification)}</small>
            </span>
          </div>
          <div className="externalKeyNotice">
            <KeyRound aria-hidden />
            <span>
              Lios 不会把此密钥复制到 ~/.lios。导入后请勿移动、改名或删除所选文件。
            </span>
          </div>
          {dialog.error && (
            <div className="rebuildError" role="alert">
              <AlertTriangle aria-hidden />
              <span>
                <strong>密钥导入失败</strong>
                <small>{dialog.error}</small>
              </span>
            </div>
          )}
        </div>
        <div className="modalActions">
          <button type="button" onClick={onClose} disabled={dialog.importing}>
            取消
          </button>
          <button
            type="button"
            className="primary"
            onClick={onConfirm}
            disabled={dialog.importing}
          >
            <KeyRound aria-hidden />
            <span>{dialog.importing ? "正在重新验证" : "确认导入"}</span>
          </button>
        </div>
      </section>
    </div>
  );
}
