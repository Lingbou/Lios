import { Download, File, RefreshCw, X } from "lucide-react";
import type { DriveItem } from "../../appTypes.ts";
import { formatBytes } from "../catalog/catalogPresentation.tsx";

export interface FilePreviewContent {
  isText: boolean;
  text?: string;
  dataUrl?: string;
}

export interface FilePreviewModalProps {
  open: boolean;
  item: DriveItem | null;
  loading: boolean;
  error: string | null;
  content: FilePreviewContent | null;
  onClose: () => void;
  onDownload: () => void;
}

export function FilePreviewModal({
  open,
  item,
  loading,
  error,
  content,
  onClose,
  onDownload
}: FilePreviewModalProps) {
  if (!open || !item) return null;

  return (
    <div className="modalBackdrop" onClick={onClose}>
      <section
        className="previewModal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="preview-modal-title"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modalHeader">
          <div className="previewTitleGroup">
            <File className="itemIcon fileIcon" aria-hidden />
            <div>
              <h2 id="preview-modal-title">{item.name}</h2>
              <p>{formatBytes(item.size)}</p>
            </div>
          </div>
          <div className="previewHeaderActions">
            <button
              type="button"
              className="previewDownloadBtn"
              onClick={onDownload}
              title="下载到本地"
            >
              <Download aria-hidden />
              <span>下载</span>
            </button>
            <button type="button" onClick={onClose} title="关闭 (Esc)" aria-label="关闭">
              <X aria-hidden />
            </button>
          </div>
        </div>

        <div className="previewModalBody">
          {loading && (
            <div className="previewLoadingState">
              <RefreshCw className="loadingGlyph" aria-hidden />
              <span>正在解密并加载预览...</span>
            </div>
          )}

          {error && !loading && (
            <div className="previewErrorState">
              <p>无法生成此文件的在线预览：{error}</p>
              <button type="button" className="primary" onClick={onDownload}>
                <Download aria-hidden />
                <span>直接下载查看</span>
              </button>
            </div>
          )}

          {!loading && !error && content && (
            <div className="previewContentContainer">
              {content.dataUrl && !content.isText && (
                <div className="imagePreviewWrapper">
                  <img src={content.dataUrl} alt={item.name} className="previewImage" />
                </div>
              )}
              {content.isText && content.text !== undefined && (
                <div className="textPreviewWrapper">
                  <pre className="textPreviewCode">
                    <code>{content.text}</code>
                  </pre>
                </div>
              )}
            </div>
          )}

          {!loading && !error && !content && (
            <div className="previewUnsupportedState">
              <p>此格式暂不支持在线即时预览</p>
              <button type="button" className="primary" onClick={onDownload}>
                <Download aria-hidden />
                <span>下载到本地查看</span>
              </button>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
