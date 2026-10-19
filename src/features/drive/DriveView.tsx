import React from "react";
import {
  AlertTriangle,
  FolderOpen,
  HardDrive,
  RefreshCw,
  Settings,
  ShieldCheck,
  UploadCloud
} from "lucide-react";
import type { CatalogStatus, CatalogTreeNode, DriveItem, SpaceSummary } from "../../appTypes";
import type {
  ContextMenuState,
  SortDirection,
  SortField,
  ViewMode
} from "./driveTypes";
import { DragDropOverlay } from "./DragDropOverlay";
import { DriveToolbar } from "./DriveToolbar";
import { FileGrid } from "./FileGrid";
import { FileTable } from "./FileTable";

export interface DriveViewProps {
  catalogTree: CatalogTreeNode | null;
  catalogStatus: CatalogStatus;
  activeSpace: SpaceSummary | null;
  currentFolder: CatalogTreeNode | null;
  currentFolderId: string | null;
  emptyDriveMode: "create" | "connect" | "select";
  selectedCount: number;
  visibleItemsCount: number;
  sortedItems: DriveItem[];
  selectedIds: Set<string>;
  viewMode: ViewMode;
  sortField: SortField;
  sortDirection: SortDirection;
  appliedQuery: string;
  busy: boolean;
  isDragging: boolean;
  onDragOver: (event: React.DragEvent) => void;
  onDragLeave: (event: React.DragEvent) => void;
  onDrop: (event: React.DragEvent) => void;
  onUploadFiles: () => void;
  onUploadFolder: () => void;
  onNewFolder: () => void;
  onDownload: () => void;
  onRename: () => void;
  onDelete: () => void;
  onRefresh: () => void;
  onClearSelection: () => void;
  onViewModeChange: (mode: ViewMode) => void;
  onSort: (field: SortField) => void;
  onToggleSelect: (id: string) => void;
  onSelectOnly: (id: string) => void;
  onSelectRange: (id: string) => void;
  onSelectAll: () => void;
  onEnterItem: (item: DriveItem) => void;
  onContextMenu: (event: React.MouseEvent, item: DriveItem | null) => void;
  openCreateSpaceDialog: () => void;
  onOpenSettings: () => void;
  initializeActiveSpace: () => void;
}

export const DriveView: React.FC<DriveViewProps> = ({
  catalogTree,
  catalogStatus,
  activeSpace,
  currentFolder,
  emptyDriveMode,
  selectedCount,
  visibleItemsCount,
  sortedItems,
  selectedIds,
  viewMode,
  sortField,
  sortDirection,
  appliedQuery,
  busy,
  isDragging,
  onDragOver,
  onDragLeave,
  onDrop,
  onUploadFiles,
  onUploadFolder,
  onNewFolder,
  onDownload,
  onRename,
  onDelete,
  onRefresh,
  onClearSelection,
  onViewModeChange,
  onSort,
  onToggleSelect,
  onSelectOnly,
  onSelectRange,
  onSelectAll,
  onEnterItem,
  onContextMenu,
  openCreateSpaceDialog,
  onOpenSettings,
  initializeActiveSpace
}) => {
  return (
    <>
      <DriveToolbar
        canUpload={Boolean(catalogTree)}
        selectedCount={selectedCount}
        totalCount={visibleItemsCount}
        viewMode={viewMode}
        busy={busy}
        onUploadFiles={onUploadFiles}
        onUploadFolder={onUploadFolder}
        onNewFolder={onNewFolder}
        onDownload={onDownload}
        onRename={onRename}
        onDelete={onDelete}
        onRefresh={onRefresh}
        onClearSelection={onClearSelection}
        onViewModeChange={onViewModeChange}
      />

      <section
        className="fileSurface"
        onDragOver={onDragOver}
        onDragLeave={onDragLeave}
        onDrop={onDrop}
      >
        <DragDropOverlay
          isDragging={isDragging}
          targetDirName={currentFolder?.name || activeSpace?.title || activeSpace?.dataset}
        />

        {!activeSpace ? (
          <div className="emptyDrive">
            {emptyDriveMode === "create" ? (
              <HardDrive aria-hidden />
            ) : (
              <UploadCloud aria-hidden />
            )}
            <h2>
              {emptyDriveMode === "create"
                ? "创建一个空间"
                : emptyDriveMode === "connect"
                  ? "连接 ModelScope"
                  : "选择一个空间"}
            </h2>
            {emptyDriveMode !== "select" && (
              <button
                className="primary"
                onClick={emptyDriveMode === "create" ? openCreateSpaceDialog : onOpenSettings}
              >
                {emptyDriveMode === "create" ? (
                  <UploadCloud aria-hidden />
                ) : (
                  <Settings aria-hidden />
                )}
                <span>{emptyDriveMode === "create" ? "创建空间" : "设置令牌"}</span>
              </button>
            )}
          </div>
        ) : catalogStatus === "loading" || catalogStatus === "idle" ? (
          <div className="emptyDrive">
            <RefreshCw className="loadingGlyph" aria-hidden />
            <h2>正在打开空间</h2>
          </div>
        ) : catalogStatus === "missing" ? (
          <div className="emptyDrive">
            <HardDrive aria-hidden />
            <h2>{activeSpace.title || activeSpace.dataset}</h2>
            <button
              className="primary"
              onClick={initializeActiveSpace}
              disabled={busy}
            >
              <ShieldCheck aria-hidden />
              初始化空间
            </button>
          </div>
        ) : catalogStatus === "error" || !catalogTree ? (
          <div className="emptyDrive">
            <AlertTriangle aria-hidden />
            <h2>打开空间失败</h2>
            <button
              className="primary"
              onClick={onRefresh}
              disabled={busy}
            >
              <RefreshCw aria-hidden />
              重试
            </button>
          </div>
        ) : visibleItemsCount === 0 ? (
          <div className="emptyDrive">
            <FolderOpen aria-hidden />
            <h2>{appliedQuery ? "没有搜索结果" : "此文件夹为空"}</h2>
          </div>
        ) : viewMode === "grid" ? (
          <FileGrid
            items={sortedItems}
            selectedIds={selectedIds}
            onToggleSelect={onToggleSelect}
            onSelectOnly={onSelectOnly}
            onSelectRange={onSelectRange}
            onEnterItem={onEnterItem}
            onContextMenu={onContextMenu}
          />
        ) : (
          <FileTable
            items={sortedItems}
            selectedIds={selectedIds}
            sortField={sortField}
            sortDirection={sortDirection}
            onSort={onSort}
            onToggleSelect={onToggleSelect}
            onSelectOnly={onSelectOnly}
            onSelectRange={onSelectRange}
            onSelectAll={onSelectAll}
            onEnterItem={onEnterItem}
            onContextMenu={onContextMenu}
          />
        )}
      </section>
    </>
  );
};
