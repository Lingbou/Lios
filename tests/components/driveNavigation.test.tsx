import { describe, expect, it } from "vitest";
import {
  calculateNextArrowIndex,
  getGridColumnCount
} from "../../src/features/drive/driveNavigation.ts";

describe("driveNavigation", () => {
  describe("calculateNextArrowIndex in table view", () => {
    it("returns null for empty items or non-up/down keys", () => {
      expect(
        calculateNextArrowIndex({
          key: "ArrowUp",
          viewMode: "table",
          currentIndex: 0,
          totalItems: 0
        })
      ).toBeNull();

      expect(
        calculateNextArrowIndex({
          key: "ArrowLeft",
          viewMode: "table",
          currentIndex: 1,
          totalItems: 5
        })
      ).toBeNull();

      expect(
        calculateNextArrowIndex({
          key: "ArrowRight",
          viewMode: "table",
          currentIndex: 1,
          totalItems: 5
        })
      ).toBeNull();
    });

    it("handles navigation when nothing is selected", () => {
      expect(
        calculateNextArrowIndex({
          key: "ArrowDown",
          viewMode: "table",
          currentIndex: -1,
          totalItems: 5
        })
      ).toBe(0);

      expect(
        calculateNextArrowIndex({
          key: "ArrowUp",
          viewMode: "table",
          currentIndex: -1,
          totalItems: 5
        })
      ).toBe(4);
    });

    it("moves up and down within bounds", () => {
      expect(
        calculateNextArrowIndex({
          key: "ArrowDown",
          viewMode: "table",
          currentIndex: 2,
          totalItems: 5
        })
      ).toBe(3);

      expect(
        calculateNextArrowIndex({
          key: "ArrowDown",
          viewMode: "table",
          currentIndex: 4,
          totalItems: 5
        })
      ).toBe(4);

      expect(
        calculateNextArrowIndex({
          key: "ArrowUp",
          viewMode: "table",
          currentIndex: 2,
          totalItems: 5
        })
      ).toBe(1);

      expect(
        calculateNextArrowIndex({
          key: "ArrowUp",
          viewMode: "table",
          currentIndex: 0,
          totalItems: 5
        })
      ).toBe(0);
    });
  });

  describe("calculateNextArrowIndex in grid view", () => {
    it("returns null for unsupported keys or empty items", () => {
      expect(
        calculateNextArrowIndex({
          key: "Enter",
          viewMode: "grid",
          currentIndex: 0,
          totalItems: 5
        })
      ).toBeNull();

      expect(
        calculateNextArrowIndex({
          key: "ArrowRight",
          viewMode: "grid",
          currentIndex: 0,
          totalItems: 0
        })
      ).toBeNull();
    });

    it("handles navigation when nothing is selected", () => {
      expect(
        calculateNextArrowIndex({
          key: "ArrowRight",
          viewMode: "grid",
          currentIndex: -1,
          totalItems: 5,
          gridColumns: 3
        })
      ).toBe(0);

      expect(
        calculateNextArrowIndex({
          key: "ArrowDown",
          viewMode: "grid",
          currentIndex: -1,
          totalItems: 5,
          gridColumns: 3
        })
      ).toBe(0);

      expect(
        calculateNextArrowIndex({
          key: "ArrowLeft",
          viewMode: "grid",
          currentIndex: -1,
          totalItems: 5,
          gridColumns: 3
        })
      ).toBe(4);

      expect(
        calculateNextArrowIndex({
          key: "ArrowUp",
          viewMode: "grid",
          currentIndex: -1,
          totalItems: 5,
          gridColumns: 3
        })
      ).toBe(4);
    });

    it("moves left and right across items", () => {
      expect(
        calculateNextArrowIndex({
          key: "ArrowRight",
          viewMode: "grid",
          currentIndex: 1,
          totalItems: 6,
          gridColumns: 3
        })
      ).toBe(2);

      expect(
        calculateNextArrowIndex({
          key: "ArrowRight",
          viewMode: "grid",
          currentIndex: 5,
          totalItems: 6,
          gridColumns: 3
        })
      ).toBe(5);

      expect(
        calculateNextArrowIndex({
          key: "ArrowLeft",
          viewMode: "grid",
          currentIndex: 2,
          totalItems: 6,
          gridColumns: 3
        })
      ).toBe(1);

      expect(
        calculateNextArrowIndex({
          key: "ArrowLeft",
          viewMode: "grid",
          currentIndex: 0,
          totalItems: 6,
          gridColumns: 3
        })
      ).toBe(0);
    });

    it("moves up and down by column count", () => {
      // Columns: 3. Grid indices:
      // [0, 1, 2]
      // [3, 4, 5]
      // [6]
      expect(
        calculateNextArrowIndex({
          key: "ArrowDown",
          viewMode: "grid",
          currentIndex: 1,
          totalItems: 7,
          gridColumns: 3
        })
      ).toBe(4);

      expect(
        calculateNextArrowIndex({
          key: "ArrowDown",
          viewMode: "grid",
          currentIndex: 4,
          totalItems: 7,
          gridColumns: 3
        })
      ).toBe(6);

      expect(
        calculateNextArrowIndex({
          key: "ArrowDown",
          viewMode: "grid",
          currentIndex: 6,
          totalItems: 7,
          gridColumns: 3
        })
      ).toBe(6);

      expect(
        calculateNextArrowIndex({
          key: "ArrowUp",
          viewMode: "grid",
          currentIndex: 4,
          totalItems: 7,
          gridColumns: 3
        })
      ).toBe(1);

      expect(
        calculateNextArrowIndex({
          key: "ArrowUp",
          viewMode: "grid",
          currentIndex: 1,
          totalItems: 7,
          gridColumns: 3
        })
      ).toBe(0);
    });
  });

  describe("getGridColumnCount", () => {
    it("defaults to 1 when no grid elements exist", () => {
      expect(getGridColumnCount()).toBe(1);
    });
  });
});
