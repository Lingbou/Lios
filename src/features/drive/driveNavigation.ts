import type { ViewMode } from "./driveTypes.ts";

export interface CalculateNextArrowIndexOptions {
  key: string;
  viewMode: ViewMode;
  currentIndex: number;
  totalItems: number;
  gridColumns?: number;
}

export function calculateNextArrowIndex({
  key,
  viewMode,
  currentIndex,
  totalItems,
  gridColumns = 1
}: CalculateNextArrowIndexOptions): number | null {
  if (totalItems <= 0) return null;

  if (viewMode === "table") {
    if (key !== "ArrowUp" && key !== "ArrowDown") {
      return null;
    }
    if (currentIndex < 0 || currentIndex >= totalItems) {
      return key === "ArrowDown" ? 0 : totalItems - 1;
    }
    if (key === "ArrowUp") {
      return Math.max(0, currentIndex - 1);
    }
    return Math.min(totalItems - 1, currentIndex + 1);
  }

  if (viewMode === "grid") {
    if (key !== "ArrowUp" && key !== "ArrowDown" && key !== "ArrowLeft" && key !== "ArrowRight") {
      return null;
    }
    if (currentIndex < 0 || currentIndex >= totalItems) {
      return key === "ArrowDown" || key === "ArrowRight" ? 0 : totalItems - 1;
    }
    const cols = Math.max(1, gridColumns);
    switch (key) {
      case "ArrowLeft":
        return Math.max(0, currentIndex - 1);
      case "ArrowRight":
        return Math.min(totalItems - 1, currentIndex + 1);
      case "ArrowUp":
        return Math.max(0, currentIndex - cols);
      case "ArrowDown":
        return Math.min(totalItems - 1, currentIndex + cols);
      default:
        return null;
    }
  }

  return null;
}

export function getGridColumnCount(): number {
  if (typeof document === "undefined") return 1;

  const cards = document.querySelectorAll<HTMLElement>(".fileGridCard");
  if (cards.length >= 2) {
    const firstTop = cards[0].offsetTop;
    let cols = 0;
    for (let i = 0; i < cards.length; i++) {
      if (cards[i].offsetTop === firstTop) {
        cols++;
      } else {
        break;
      }
    }
    if (cols > 0 && cols < cards.length) {
      return cols;
    }
    if (cards[cards.length - 1].offsetLeft > cards[0].offsetLeft) {
      return cards.length;
    }
  }

  const gridEl = document.querySelector<HTMLElement>(".fileGrid");
  if (gridEl && typeof window !== "undefined") {
    const template = window.getComputedStyle(gridEl).gridTemplateColumns;
    if (template && !template.includes("none")) {
      const parts = template.split(" ").filter(Boolean);
      if (parts.length > 0) return parts.length;
    }
  }

  return 1;
}
