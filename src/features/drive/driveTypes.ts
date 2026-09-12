import type { DriveItem } from "../../appTypes.ts";

export type SortField = "name" | "kind" | "size" | "updated_at";
export type SortDirection = "asc" | "desc";
export type ViewMode = "table" | "grid";

export type ContextMenuState = {
  open: boolean;
  x: number;
  y: number;
  item: DriveItem | null;
};
