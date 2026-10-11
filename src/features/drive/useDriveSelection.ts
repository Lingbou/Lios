import { useCallback, useState } from "react";
import type { DriveItem } from "../../appTypes.ts";

export function useDriveSelection(sortedItems: DriveItem[]) {
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [lastSelectedId, setLastSelectedId] = useState<string | null>(null);

  const toggleSelection = useCallback((id: string) => {
    setLastSelectedId(id);
    setSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const selectOnly = useCallback((id: string) => {
    setLastSelectedId(id);
    setSelectedIds(new Set([id]));
  }, []);

  const selectRange = useCallback(
    (targetId: string) => {
      if (!lastSelectedId || lastSelectedId === targetId) {
        selectOnly(targetId);
        return;
      }
      const lastIdx = sortedItems.findIndex((item) => item.id === lastSelectedId);
      const targetIdx = sortedItems.findIndex((item) => item.id === targetId);
      if (lastIdx === -1 || targetIdx === -1) {
        selectOnly(targetId);
        return;
      }
      const start = Math.min(lastIdx, targetIdx);
      const end = Math.max(lastIdx, targetIdx);
      const rangeIds = sortedItems.slice(start, end + 1).map((item) => item.id);
      setSelectedIds(new Set(rangeIds));
    },
    [lastSelectedId, selectOnly, sortedItems]
  );

  const selectAll = useCallback(() => {
    if (sortedItems.length === 0) return;
    if (sortedItems.every((item) => selectedIds.has(item.id))) {
      setSelectedIds(new Set());
    } else {
      setSelectedIds(new Set(sortedItems.map((item) => item.id)));
    }
  }, [selectedIds, sortedItems]);

  const clearSelection = useCallback(() => {
    setSelectedIds(new Set());
    setLastSelectedId(null);
  }, []);

  return {
    selectedIds,
    setSelectedIds,
    lastSelectedId,
    setLastSelectedId,
    toggleSelection,
    selectOnly,
    selectRange,
    selectAll,
    clearSelection
  };
}