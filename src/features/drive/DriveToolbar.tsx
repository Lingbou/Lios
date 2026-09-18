import {
  Download,
  Edit3,
  FolderOpen,
  LayoutGrid,
  List,
  Plus,
  RefreshCw,
  Trash2,
  UploadCloud
} from "lucide-react";
import type { ViewMode } from "./driveTypes.ts";

interface DriveToolbarProps {
  canUpload: boolean;
  selectedCount: number;
  totalCount: number;
  viewMode: ViewMode;
  busy: boolean;
  onUploadFiles: () => void;
  onUploadFolder: () => void;
  onNewFolder: () => void;
  onDownload: () => void;
  onRename: () => void;
  onDelete: () => void;
  onRefresh: () => void;
  onClearSelection: () => void;
  onViewModeChange: (mode: ViewMode) => void;
}

export function DriveToolbar({
  canUpload,
  selectedCount,
  totalCount,
  viewMode,
  busy,
  onUploadFiles,
  onUploadFolder,
  onNewFolder,
  onDownload,
  onRename,
  onDelete,
  onRefresh,
  onClearSelection,
  onViewModeChange
}: DriveToolbarProps) {
  return (
    <section className="driveToolbar">
      <div className="toolbar">
        <button
          className="primary"
          onClick={onUploadFiles}
          disabled={!canUpload || busy}
          title="上传文件 (支持拖拽)"
        >
          <UploadCloud aria-hidden />
          <span>上传文件</span>
        </button>
        <button
          onClick={onUploadFolder}
          disabled={!canUpload || busy}
          title="上传文件夹"
        >
          <FolderOpen aria-hidden />
          <span>上传文件夹</span>
        </button>
        <button
          onClick={onNewFolder}
          disabled={!canUpload || busy}
          title="新建文件夹"
        >
          <Plus aria-hidden />
          <span>新建文件夹</span>
        </button>
      </div>

      <div className="toolbar">
        <button
          onClick={onDownload}
          disabled={selectedCount === 0 || busy}
          title={selectedCount > 0 ? `下载已选 (${selectedCount})` : "下载"}
        >
          <Download aria-hidden />
          <span>下载</span>
        </button>
        <button
          onClick={onRename}
          disabled={selectedCount !== 1 || busy}
          title="重命名 (F2)"
        >
          <Edit3 aria-hidden />
          <span>重命名</span>
        </button>
        <button
          className="danger"
          onClick={onDelete}
          disabled={selectedCount === 0 || busy}
          title="删除已选 (Delete)"
        >
          <Trash2 aria-hidden />
          <span>删除</span>
        </button>
        <button
          onClick={onRefresh}
          disabled={busy}
          title="刷新目录"
        >
          <RefreshCw aria-hidden />
          <span>刷新</span>
        </button>

        <div className="toolbarDivider" />

        {selectedCount > 0 && (
          <div className="selectionBadge">
            <span>已选 {selectedCount} / {totalCount} 项</span>
            <button
              type="button"
              className="clearSelectionBtn"
              onClick={onClearSelection}
              title="取消选择 (Esc)"
            >
              取消
            </button>
          </div>
        )}

        <div className="viewModeSegment" role="radiogroup" aria-label="视图模式">
          <button
            type="button"
            className={`viewModeBtn ${viewMode === "table" ? "active" : ""}`}
            onClick={() => onViewModeChange("table")}
            title="列表视图"
            aria-checked={viewMode === "table"}
            role="radio"
          >
            <List aria-hidden />
          </button>
          <button
            type="button"
            className={`viewModeBtn ${viewMode === "grid" ? "active" : ""}`}
            onClick={() => onViewModeChange("grid")}
            title="网格平铺视图"
            aria-checked={viewMode === "grid"}
            role="radio"
          >
            <LayoutGrid aria-hidden />
          </button>
        </div>
      </div>
    </section>
  );
}
