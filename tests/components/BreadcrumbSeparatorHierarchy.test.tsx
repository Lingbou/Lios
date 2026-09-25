import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, expect, it, vi, beforeEach } from "vitest";
import App from "../../src/App.tsx";
import type { CatalogLoadResult, Snapshot } from "../../src/appTypes.ts";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn().mockResolvedValue("0.1.0") }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(), save: vi.fn() }));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: vi.fn(() => ({ onDragDropEvent: vi.fn().mockResolvedValue(() => {}) }))
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: vi.fn(() => ({
    minimize: vi.fn(),
    toggleMaximize: vi.fn(),
    close: vi.fn(),
    startDragging: vi.fn()
  }))
}));

const nestedTree = {
  id: "root",
  name: "root",
  updated_at: "2026-01-01T00:00:00Z",
  kind: {
    type: "Directory" as const,
    children: [
      {
        id: "folder-1",
        name: "parent-folder",
        updated_at: "2026-01-01T00:00:00Z",
        kind: {
          type: "Directory" as const,
          children: [
            {
              id: "folder-2",
              name: "child-folder",
              updated_at: "2026-01-01T00:00:00Z",
              kind: {
                type: "Directory" as const,
                children: []
              }
            }
          ]
        }
      }
    ]
  }
};

describe("Breadcrumb separator hierarchy", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("renders separators as siblings between buttons rather than nested inside buttons", async () => {
    const { invoke } = await import("@tauri-apps/api/core");
    vi.mocked(invoke).mockImplementation(async (cmd, args?: any) => {
      if (cmd === "current_setup") {
        return {
          paths: { config_dir: "", cache_dir: "", staging_dir: "", logs: "", credentials: "" },
          config: { active_space: null, modelscope_endpoint: "" },
          recovery_key: { has_key: true, location: "" },
          has_token: true,
          spaces: [
            {
              space_name: "demo-space",
              namespace: "testuser",
              dataset: "demo-dataset",
              endpoint: "https://modelscope.cn",
              local_path: "/dummy",
              created_at: "2026-01-01T00:00:00Z"
            }
          ]
        } as unknown as Snapshot;
      }
      if (cmd === "list_dataset_repos") {
        return { user: { username: "testuser" }, repositories: [] };
      }
      if (cmd === "list_tasks") {
        return [];
      }
      if (cmd === "load_space_catalog") {
        return {
          local_path: "/dummy",
          bytes: 100,
          tree: nestedTree,
          warnings: []
        } as CatalogLoadResult;
      }
      return null;
    });

    render(<App />);

    // Enter Drive view
    const spaceCard = await screen.findByText("demo-dataset");
    fireEvent.click(spaceCard);
    expect(await screen.findByText("parent-folder")).toBeInTheDocument();

    // Enter parent-folder
    fireEvent.click(screen.getByRole("button", { name: "parent-folder" }));
    expect(await screen.findByText("child-folder")).toBeInTheDocument();

    // Enter child-folder
    fireEvent.click(screen.getByRole("button", { name: "child-folder" }));
    expect(await screen.findByText("此文件夹为空")).toBeInTheDocument();

    // Now crumbs nav has: [rootBtn ("空间列表"), crumb0 ("demo-dataset"), crumb1 ("parent-folder"), crumb2 ("child-folder")]
    const nav = screen.getByRole("navigation", { name: "当前路径" });
    const crumbButtons = nav.querySelectorAll("button:not(.crumbRootBtn)");
    expect(crumbButtons.length).toBe(3);

    // None of the crumb buttons should contain an SVG icon (ChevronRight)
    for (const btn of crumbButtons) {
      const svgs = btn.querySelectorAll("svg");
      expect(svgs.length).toBe(0);
    }

    // All separators should have class crumbSeparator and exist outside the crumb buttons
    const separators = nav.querySelectorAll(".crumbSeparator");
    // 1 between rootBtn and crumb0, 1 between crumb0 and crumb1, 1 between crumb1 and crumb2 -> total 3 separators
    expect(separators.length).toBe(3);
    for (const sep of separators) {
      expect(sep.closest("button")).toBeNull();
    }
  });
});
