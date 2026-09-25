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

const initialTree = {
  id: "root",
  name: "root",
  updated_at: "2026-01-01T00:00:00Z",
  kind: {
    type: "Directory" as const,
    children: [
      {
        id: "folder-deleted",
        name: "deleted-folder",
        updated_at: "2026-01-01T00:00:00Z",
        kind: {
          type: "Directory" as const,
          children: []
        }
      }
    ]
  }
};

const updatedTree = {
  id: "root",
  name: "root",
  updated_at: "2026-01-02T00:00:00Z",
  kind: {
    type: "Directory" as const,
    children: [
      {
        id: "folder-new",
        name: "new-folder",
        updated_at: "2026-01-02T00:00:00Z",
        kind: {
          type: "Directory" as const,
          children: []
        }
      }
    ]
  }
};

describe("Catalog reload fallback", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("falls back to root folder when current folder no longer exists after reload", async () => {
    let currentTree = initialTree;

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
          tree: currentTree,
          warnings: []
        } as CatalogLoadResult;
      }
      return null;
    });

    render(<App />);

    // Enter Drive view
    const spaceCard = await screen.findByText("demo-dataset");
    fireEvent.click(spaceCard);
    expect(await screen.findByText("deleted-folder")).toBeInTheDocument();

    // Enter the folder that will be deleted
    fireEvent.doubleClick(screen.getByText("deleted-folder"));
    expect(await screen.findByText("此文件夹为空")).toBeInTheDocument();

    // Now update the tree on server (deleted-folder is gone, replaced with new-folder)
    currentTree = updatedTree;

    // Trigger reload via the toolbar Refresh button
    const refreshBtn = screen.getByRole("button", { name: "刷新" });
    fireEvent.click(refreshBtn);

    // After reload, since current folder is gone, it should smoothly fall back to root, showing new-folder
    expect(await screen.findByText("new-folder")).toBeInTheDocument();
  });
});
