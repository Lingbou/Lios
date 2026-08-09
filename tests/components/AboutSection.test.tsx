import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AboutSection } from "../../src/features/settings/AboutSection.tsx";

describe("AboutSection", () => {
  it("shows the version reported by the running desktop application", async () => {
    const loadVersion = vi.fn().mockResolvedValue("0.1.0");

    render(<AboutSection loadVersion={loadVersion} />);

    expect(screen.getByRole("heading", { name: "关于 Lios" })).toBeInTheDocument();
    expect(await screen.findByText("v0.1.0")).toBeInTheDocument();
    expect(screen.getByText("干翻百度网盘")).toBeInTheDocument();
    expect(screen.getByText("MIT")).toBeInTheDocument();
    expect(screen.getByText("GitHub")).toBeInTheDocument();
    expect(screen.getByText("Lingbou")).toHaveAttribute("title", "https://github.com/Lingbou");
    expect(screen.queryByText("Desktop 与 CLI 使用同一版本号")).not.toBeInTheDocument();
    expect(loadVersion).toHaveBeenCalledOnce();
  });

  it("reports an unavailable version without hiding the rest of the product information", async () => {
    const loadVersion = vi.fn().mockRejectedValue(new Error("runtime unavailable"));

    render(<AboutSection loadVersion={loadVersion} />);

    expect(await screen.findByText("版本未知")).toBeInTheDocument();
    expect(screen.getByText("MIT")).toBeInTheDocument();
  });
});
