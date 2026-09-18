import {
  CheckSquare,
  Download,
  Edit3,
  Eye,
  FolderOpen,
  Plus,
  RefreshCw,
  Trash2,
  UploadCloud
} from "lucide-react";
import { useEffect, useLayoutEffect, useRef } from "react";
import type { DriveItem } from "../../appTypes.ts";
import type { ContextMenuState } from "./driveTypes.ts";

interface ContextMenuProps {
  state: ContextMenuState;
  onClose: () => void;
  selectedCount: number;
  onOpenItem?: (item: DriveItem) => void;
  onPreviewItem?: (item: DriveItem) => void;
  onDownload?: () => void;
  onRename?: () => void;
  onDelete?: () => void;
  onUploadFiles?: () => void;
  onUploadFolder?: () => void;
  onNewFolder?: () => void;
  onRefresh?: () => void;
  onSelectAll?: () => void;
}

export function ContextMenu({
  state,
  onClose,
  selectedCount,
  onOpenItem,
  onPreviewItem,
  onDownload,
  onRename,
  onDelete,
  onUploadFiles,
  onUploadFolder,
  onNewFolder,
  onRefresh,
  onSelectAll
}: ContextMenuProps) {
  const menuRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (!state.open || !menuRef.current) return;
    const el = menuRef.current;
    const menuWidth = 190;
    const menuHeight = 220;
    const viewportWidth = window.innerWidth;
    const viewportHeight = window.innerHeight;

    let posX = state.x;
    let posY = state.y;

    if (posX + menuWidth > viewportWidth) {
      posX = Math.max(10, viewportWidth - menuWidth - 10);
    }
    if (posY + menuHeight > viewportHeight) {
      posY = Math.max(10, viewportHeight - menuHeight - 10);
    }

    el.style.left = `${posX}px`;
    el.style.top = `${posY}px`;
  }, [state.open, state.x, state.y]);

  useEffect(() => {
    if (!state.open) return;

    function handlePointerDown(event: PointerEvent) {
      if (menuRef.current && !menuRef.current.contains(event.target as Node)) {
        onClose();
      }
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        onClose();
      }
    }

    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("scroll", onClose, true);

    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("scroll", onClose, true);
    };
  }, [state.open, onClose]);

  if (!state.open) return null;

  const item = state.item;

  return (
    <div
      ref={menuRef}
      className="contextMenu"
      role="menu"
      aria-label="操作菜单"
      onContextMenu={(event) => event.preventDefault()}
    >
      {item ? (
        <>
          <div className="contextMenuHeader">
            <span className="contextMenuTargetName">{item.name}</span>
          </div>
          {item.kind === "Directory" && onOpenItem && (
            <button
              className="contextMenuItem"
              role="menuitem"
              onClick={() => {
                onOpenItem(item);
                onClose();
              }}
            >
              <FolderOpen aria-hidden />
              <span>打开文件夹</span>
            </button>
          )}
          {item.kind === "File" && onPreviewItem && (
            <button
              className="contextMenuItem"
              role="menuitem"
              onClick={() => {
                onPreviewItem(item);
                onClose();
              }}
            >
              <Eye aria-hidden />
              <span>在线预览</span>
            </button>
          )}
          {onDownload && (
            <button
              className="contextMenuItem"
              role="menuitem"
              onClick={() => {
                onDownload();
                onClose();
              }}
            >
              <Download aria-hidden />
              <span>下载{selectedCount > 1 ? ` (${selectedCount})` : ""}</span>
            </button>
          )}
          {selectedCount === 1 && onRename && (
            <button
              className="contextMenuItem"
              role="menuitem"
              onClick={() => {
                onRename();
                onClose();
              }}
            >
              <Edit3 aria-hidden />
              <span>重命名</span>
            </button>
          )}
          <div className="contextMenuDivider" />
          {onDelete && (
            <button
              className="contextMenuItem danger"
              role="menuitem"
              onClick={() => {
                onDelete();
                onClose();
              }}
            >
              <Trash2 aria-hidden />
              <span>删除{selectedCount > 1 ? ` (${selectedCount})` : ""}</span>
            </button>
          )}
        </>
      ) : (
        <>
          {onUploadFiles && (
            <button
              className="contextMenuItem"
              role="menuitem"
              onClick={() => {
                onUploadFiles();
                onClose();
              }}
            >
              <UploadCloud aria-hidden />
              <span>上传文件</span>
            </button>
          )}
          {onUploadFolder && (
            <button
              className="contextMenuItem"
              role="menuitem"
              onClick={() => {
                onUploadFolder();
                onClose();
              }}
            >
              <FolderOpen aria-hidden />
              <span>上传文件夹</span>
            </button>
          )}
          {onNewFolder && (
            <button
              className="contextMenuItem"
              role="menuitem"
              onClick={() => {
                onNewFolder();
                onClose();
              }}
            >
              <Plus aria-hidden />
              <span>新建文件夹</span>
            </button>
          )}
          <div className="contextMenuDivider" />
          {onSelectAll && (
            <button
              className="contextMenuItem"
              role="menuitem"
              onClick={() => {
                onSelectAll();
                onClose();
              }}
            >
              <CheckSquare aria-hidden />
              <span>全选</span>
            </button>
          )}
          {onRefresh && (
            <button
              className="contextMenuItem"
              role="menuitem"
              onClick={() => {
                onRefresh();
                onClose();
              }}
            >
              <RefreshCw aria-hidden />
              <span>刷新</span>
            </button>
          )}
        </>
      )}
    </div>
  );
}
