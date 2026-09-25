import { getFileCategory } from "./fileCategory.ts";
import { ChevronDown, ChevronUp, File, Folder } from "lucide-react";
import { type MouseEvent, memo, useEffect, useRef } from "react";
import type { DriveItem } from "../../appTypes.ts";
import { formatBytes, formatDate } from "../catalog/catalogPresentation.tsx";
import type { SortDirection, SortField } from "./driveTypes.ts";

interface FileTableProps {
  items: DriveItem[];
  selectedIds: Set<string>;
  sortField: SortField;
  sortDirection: SortDirection;
  onSort: (field: SortField) => void;
  onToggleSelect: (id: string) => void;
  onSelectOnly: (id: string) => void;
  onSelectRange: (targetId: string) => void;
  onSelectAll: () => void;
  onEnterItem: (item: DriveItem) => void;
  onContextMenu: (event: MouseEvent, item: DriveItem | null) => void;
}

function FileTableComponent({
  items,
  selectedIds,
  sortField,
  sortDirection,
  onSort,
  onToggleSelect,
  onSelectOnly,
  onSelectRange,
  onSelectAll,
  onEnterItem,
  onContextMenu
}: FileTableProps) {
  const headerCheckboxRef = useRef<HTMLInputElement>(null);

  const allSelected = items.length > 0 && items.every((item) => selectedIds.has(item.id));
  const someSelected = items.some((item) => selectedIds.has(item.id)) && !allSelected;

  useEffect(() => {
    if (headerCheckboxRef.current) {
      headerCheckboxRef.current.indeterminate = someSelected;
    }
  }, [someSelected]);

  function handleRowClick(event: MouseEvent, item: DriveItem) {
    // If clicking directly on a button or checkbox, let their own handlers handle it
    if ((event.target as HTMLElement).closest("button, input")) return;

    if (event.shiftKey) {
      onSelectRange(item.id);
    } else if (event.ctrlKey || event.metaKey) {
      onToggleSelect(item.id);
    } else {
      onSelectOnly(item.id);
    }
  }

  function handleRowContextMenu(event: MouseEvent, item: DriveItem) {
    event.preventDefault();
    event.stopPropagation();
    if (!selectedIds.has(item.id)) {
      onSelectOnly(item.id);
    }
    onContextMenu(event, item);
  }

  function handleTableContextMenu(event: MouseEvent) {
    if ((event.target as HTMLElement).closest("tr.fileRow")) return;
    event.preventDefault();
    onContextMenu(event, null);
  }

  function renderSortIcon(field: SortField) {
    if (sortField !== field) return null;
    return sortDirection === "asc" ? (
      <ChevronUp className="sortIndicator" aria-hidden />
    ) : (
      <ChevronDown className="sortIndicator" aria-hidden />
    );
  }

  return (
    <div className="fileTableContainer" onContextMenu={handleTableContextMenu}>
      <table className="fileTable">
        <thead>
          <tr>
            <th className="selectCol" aria-label="全选">
              <input
                ref={headerCheckboxRef}
                type="checkbox"
                checked={allSelected}
                onChange={onSelectAll}
                title={allSelected ? "取消全选 (Ctrl+A)" : "全选 (Ctrl+A)"}
                aria-label="选择全部项目"
              />
            </th>
            <th className="sortableCol" onClick={() => onSort("name")}>
              <span>名称</span>
              {renderSortIcon("name")}
            </th>
            <th className="sortableCol" onClick={() => onSort("kind")}>
              <span>类型</span>
              {renderSortIcon("kind")}
            </th>
            <th className="sortableCol" onClick={() => onSort("size")}>
              <span>大小</span>
              {renderSortIcon("size")}
            </th>
            <th className="sortableCol" onClick={() => onSort("updated_at")}>
              <span>修改时间</span>
              {renderSortIcon("updated_at")}
            </th>
          </tr>
        </thead>
        <tbody>
          {items.map((item) => {
            const isSelected = selectedIds.has(item.id);
            const isDir = item.kind === "Directory";
            const category = !isDir ? getFileCategory(item.name) : undefined;
            return (
              <tr
                key={item.id}
                className={`fileRow ${isSelected ? "selected" : ""}`}
                onClick={(event) => handleRowClick(event, item)}
                onDoubleClick={() => onEnterItem(item)}
                onContextMenu={(event) => handleRowContextMenu(event, item)}
              >
                <td className="selectCol">
                  <input
                    type="checkbox"
                    checked={isSelected}
                    onChange={() => onToggleSelect(item.id)}
                    aria-label={`选择 ${item.name}`}
                  />
                </td>
                <td className="nameCol">
                  <button
                    type="button"
                    className="fileName"
                    onClick={(event) => {
                      event.stopPropagation();
                      onEnterItem(item);
                    }}
                    title={item.name}
                  >
                    {isDir ? (
                      <Folder className="itemIcon folderIcon" aria-hidden />
                    ) : (
                      <File className={`itemIcon fileIcon fileCategory-${category}`} aria-hidden />
                    )}
                    <span>{item.name}</span>
                  </button>
                </td>
                <td>{isDir ? `${item.children_count} 项` : "文件"}</td>
                <td>{!isDir ? formatBytes(item.size) : "-"}</td>
                <td className="dateCol">{formatDate(item.updated_at)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

export const FileTable = memo(FileTableComponent);
