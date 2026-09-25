import { describe, expect, it } from "vitest";
import { getFileCategory, getFileExtension } from "../../src/features/drive/fileCategory.ts";
import { formatDate } from "../../src/features/catalog/catalogPresentation.tsx";

describe("fileCategory", () => {
  it("extracts file extension properly", () => {
    expect(getFileExtension("test.ts")).toBe("ts");
    expect(getFileExtension("archive.tar.gz")).toBe("gz");
    expect(getFileExtension("noextension")).toBe("");
    expect(getFileExtension(".hidden")).toBe("");
  });

  it("classifies code extensions", () => {
    expect(getFileCategory("app.tsx")).toBe("code");
    expect(getFileCategory("main.py")).toBe("code");
    expect(getFileCategory("Cargo.toml")).toBe("code");
  });

  it("classifies image extensions", () => {
    expect(getFileCategory("photo.png")).toBe("image");
    expect(getFileCategory("banner.JPEG")).toBe("image");
    expect(getFileCategory("icon.svg")).toBe("image");
  });

  it("classifies doc extensions", () => {
    expect(getFileCategory("paper.pdf")).toBe("doc");
    expect(getFileCategory("notes.docx")).toBe("doc");
  });

  it("classifies sheet extensions", () => {
    expect(getFileCategory("data.csv")).toBe("sheet");
    expect(getFileCategory("finance.xlsx")).toBe("sheet");
    expect(getFileCategory("train.parquet")).toBe("sheet");
  });

  it("classifies archive extensions", () => {
    expect(getFileCategory("bundle.zip")).toBe("archive");
    expect(getFileCategory("release.tar.gz")).toBe("archive");
  });

  it("classifies media extensions", () => {
    expect(getFileCategory("video.mp4")).toBe("media");
    expect(getFileCategory("audio.mp3")).toBe("media");
  });

  it("falls back to default for unknown or missing extensions", () => {
    expect(getFileCategory("binary.xyz")).toBe("default");
    expect(getFileCategory("Dockerfile")).toBe("default");
  });
});

describe("formatDate", () => {
  it("formats date to YYYY-MM-DD HH:mm", () => {
    const formatted = formatDate("2026-08-01T10:30:00Z");
    expect(formatted).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
  });

  it("handles null and undefined", () => {
    expect(formatDate(null)).toBe("-");
    expect(formatDate(undefined)).toBe("-");
    expect(formatDate("invalid-date")).toBe("-");
  });
});
