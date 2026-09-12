import { AlertTriangle, RefreshCw, X } from "lucide-react";
import type { CatalogRebuildDialog } from "../../appTypes.ts";
import { CatalogRecoveryTree, formatBytes } from "./catalogPresentation.tsx";

export interface RebuildCatalogModalProps {
  dialog: CatalogRebuildDialog | null;
  rebuildTaskActive: boolean;
  onClose: () => void;
  onReloadPreview: () => void;
  onConfirm: () => void;
}

export function RebuildCatalogModal({
  dialog,
  rebuildTaskActive,
  onClose,
  onReloadPreview,
  onConfirm
}: RebuildCatalogModalProps) {
  if (!dialog) return null;

  return (
    <div className="modalBackdrop">
      <section
        className="rebuildModal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="rebuild-catalog-title"
      >
        <div className="modalHeader">
          <div>
            <h2 id="rebuild-catalog-title">恢复目录结构</h2>
            <p>{`${dialog.space.namespace}/${dialog.space.dataset}`}</p>
          </div>
          <button
            onClick={onClose}
            disabled={dialog.status === "submitting"}
            title="关闭"
            aria-label="关闭"
          >
            <X aria-hidden />
          </button>
        </div>

        <div className="rebuildModalBody">
          {dialog.status === "loading" && (
            <div className="rebuildLoading" role="status">
              <RefreshCw aria-hidden />
              <strong>正在准备恢复预览</strong>
              <span>正在读取可恢复的目录信息，请稍候。</span>
            </div>
          )}

          {dialog.error && (
            <div className="rebuildError" role="alert">
              <AlertTriangle aria-hidden />
              <span>
                <strong>{dialog.preview ? "恢复任务未能开始" : "无法生成恢复预览"}</strong>
                <small>{dialog.error}</small>
              </span>
            </div>
          )}

          {dialog.preview && (
            <>
              <p className="rebuildIntro">
                Lios 将根据已恢复的信息重新生成目录索引。请确认下方目录和统计无误；如果远端内容在预览后发生变化，任务会停止并要求重新预览。
              </p>

              <dl className="rebuildReport">
                <div>
                  <dt>可恢复节点</dt>
                  <dd>{dialog.preview.report.nodes_rebuilt}</dd>
                </div>
                <div>
                  <dt>文件夹</dt>
                  <dd>{dialog.preview.report.directories_rebuilt}</dd>
                </div>
                <div>
                  <dt>文件</dt>
                  <dd>{dialog.preview.report.files_rebuilt}</dd>
                </div>
                <div>
                  <dt>文件内容</dt>
                  <dd>{dialog.preview.report.content_objects_rebuilt}</dd>
                </div>
                <div>
                  <dt>数据分片</dt>
                  <dd>{dialog.preview.report.chunks_referenced}</dd>
                </div>
                <div>
                  <dt>原始数据</dt>
                  <dd>{formatBytes(dialog.preview.report.original_bytes_referenced)}</dd>
                </div>
                <div>
                  <dt>未引用对象</dt>
                  <dd>{dialog.preview.report.unreferenced_managed_objects}</dd>
                </div>
              </dl>

              <section className="rebuildTreePanel" aria-labelledby="rebuild-tree-title">
                <div className="rebuildSectionHeader">
                  <h3 id="rebuild-tree-title">可恢复的目录</h3>
                  <span>完整预览</span>
                </div>
                <div className="rebuildTreeScroll">
                  <ul className="rebuildTree">
                    <CatalogRecoveryTree node={dialog.preview.tree} />
                  </ul>
                </div>
              </section>

              <section className="rebuildWarnings" aria-labelledby="rebuild-warnings-title">
                <div className="rebuildSectionHeader">
                  <h3 id="rebuild-warnings-title">恢复提示</h3>
                  <span>{dialog.preview.warnings.length} 项</span>
                </div>
                {dialog.preview.warnings.length > 0 ? (
                  <ul>
                    {dialog.preview.warnings.map((warning, index) => (
                      <li key={`${index}-${warning}`}>{warning}</li>
                    ))}
                  </ul>
                ) : (
                  <p>未发现需要额外处理的提示。</p>
                )}
              </section>
            </>
          )}
        </div>

        <div className="modalActions">
          <button
            type="button"
            onClick={onClose}
            disabled={dialog.status === "submitting"}
          >
            取消
          </button>
          <div className="modalActionGroup">
            {dialog.status === "error" && (
              <button type="button" onClick={onReloadPreview}>
                重新预览
              </button>
            )}
            <button
              type="button"
              className="primary"
              onClick={onConfirm}
              disabled={
                !dialog.preview ||
                dialog.status === "loading" ||
                dialog.status === "submitting" ||
                rebuildTaskActive
              }
            >
              <RefreshCw
                className={dialog.status === "submitting" ? "loadingGlyph" : undefined}
                aria-hidden
              />
              <span>{dialog.status === "submitting" ? "正在加入任务" : "确认恢复"}</span>
            </button>
          </div>
        </div>
      </section>
    </div>
  );
}
