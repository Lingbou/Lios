import { getFileCategory, getFileExtension } from "./fileCategory.ts";
import { File, Folder } from "lucide-react";
import { memo, type MouseEvent } from "react";
import type { DriveItem } from "../../appTypes.ts";
import { formatBytes, formatDate } from "../catalog/catalogPresentation.tsx";

interface FileGridProps {
  items: DriveItem[];
  selectedIds: Set<string>;
  onToggleSelect: (id: string) => void;
  onSelectOnly: (id: string) => void;
  onSelectRange: (targetId: string) => void;
  onEnterItem: (item: DriveItem) => void;
  onContextMenu: (event: MouseEvent, item: DriveItem | null) => void;
}



function FileGridComponent({
  items,
  selectedIds,
  onToggleSelect,
  onSelectOnly,
  onSelectRange,
  onEnterItem,
  onContextMenu
}: FileGridProps) {
  function handleItemClick(event: MouseEvent, item: DriveItem) {
    if ((event.target as HTMLElement).closest("input[type='checkbox']")) return;

    if (event.shiftKey) {
      onSelectRange(item.id);
    } else if (event.ctrlKey || event.metaKey) {
      onToggleSelect(item.id);
    } else {
      onSelectOnly(item.id);
    }
  }

  function handleItemContextMenu(event: MouseEvent, item: DriveItem) {
    event.preventDefault();
    event.stopPropagation();
    if (!selectedIds.has(item.id)) {
      onSelectOnly(item.id);
    }
    onContextMenu(event, item);
  }

  function handleGridContextMenu(event: MouseEvent) {
    if ((event.target as HTMLElement).closest(".fileGridCard")) return;
    event.preventDefault();
    onContextMenu(event, null);
  }

  return (
    <div className="fileGridContainer" onContextMenu={handleGridContextMenu}>
      <div className="fileGrid">
        {items.map((item) => {
          const isSelected = selectedIds.has(item.id);
          const isDir = item.kind === "Directory";
          const ext = !isDir ? getFileExtension(item.name).toUpperCase() : "";
          const category = !isDir ? getFileCategory(item.name) : undefined;

          return (
            <div
              key={item.id}
              className={`fileGridCard ${isSelected ? "selected" : ""} ${isDir ? "isFolder" : "isFile"}`}
              onClick={(event) => handleItemClick(event, item)}
              onDoubleClick={() => onEnterItem(item)}
              onContextMenu={(event) => handleItemContextMenu(event, item)}
              title={`${item.name}\n${isDir ? `${item.children_count} 项` : formatBytes(item.size)}\n修改时间: ${formatDate(item.updated_at)}`}
              role="button"
              tabIndex={0}
            >
              <div className="cardCheckbox">
                <input
                  type="checkbox"
                  checked={isSelected}
                  onChange={() => onToggleSelect(item.id)}
                  onClick={(event) => event.stopPropagation()}
                  aria-label={`选择 ${item.name}`}
                />
              </div>

              <div className="cardIconWrapper">
                {isDir ? (
                  <Folder className="gridItemIcon folderIcon" aria-hidden />
                ) : (
                  <div className="fileIconContainer">
                    <File className={`gridItemIcon fileIcon fileCategory-${category}`} aria-hidden />
                    {ext && <span className={`fileExtBadge badge-${category}`}>{ext.slice(0, 4)}</span>}
                  </div>
                )}
              </div>

              <div className="cardDetails">
                <span className="cardTitle">{item.name}</span>
                <span className="cardSubtitle">
                  {isDir ? `${item.children_count} 项` : formatBytes(item.size)}
                </span>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export const FileGrid = memo(FileGridComponent);
