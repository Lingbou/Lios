import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
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

const tree = {
  id: "root",
  name: "root",
  updated_at: "2026-01-01T00:00:00Z",
  kind: {
    type: "Directory" as const,
    children: [
      {
        id: "folder-1",
        name: "subfolder",
        updated_at: "2026-01-01T00:00:00Z",
        kind: {
          type: "Directory" as const,
          children: []
        }
      }
    ]
  }
};

describe("SearchBox in Drive view", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  function setupMockInvoke() {
    return vi.fn(async (cmd: string, args?: any) => {
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
          tree,
          warnings: []
        } as CatalogLoadResult;
      }
      if (cmd === "search_catalog") {
        if (args?.query === "match") {
          return [
            {
              id: "item-match",
              name: "match-file.txt",
              kind: "File",
              size: 42,
              updated_at: "2026-01-01T00:00:00Z",
              children_count: 0
            }
          ];
        }
        return [];
      }
      return null;
    });
  }

  it("retains directory items while typing and executes debounced search", async () => {
    const { invoke } = await import("@tauri-apps/api/core");
    const mockInvoke = setupMockInvoke();
    vi.mocked(invoke).mockImplementation(mockInvoke);

    render(<App />);

    // Enter Drive view
    const spaceCard = (await screen.findAllByText("demo-dataset"))[0];
    fireEvent.click(spaceCard);
    expect(await screen.findByText("subfolder")).toBeInTheDocument();

    const searchInput = screen.getByPlaceholderText("搜索当前空间") as HTMLInputElement;

    // Type into search box
    fireEvent.change(searchInput, { target: { value: "mat" } });

    // Immediately after typing, the directory list should NOT be wiped out
    expect(screen.getByText("subfolder")).toBeInTheDocument();
    expect(screen.queryByText("没有搜索结果")).not.toBeInTheDocument();

    // Clear button should be visible
    const clearBtn = screen.getByRole("button", { name: "清空搜索" });
    expect(clearBtn).toBeInTheDocument();

    // Now type "match" and wait 300ms for debounce
    fireEvent.change(searchInput, { target: { value: "match" } });

    // Wait for debounced search to trigger and show search results
    expect(await screen.findByText("match-file.txt", {}, { timeout: 1000 })).toBeInTheDocument();
    expect(screen.queryByText("subfolder")).not.toBeInTheDocument();

    // Click clear button
    fireEvent.click(clearBtn);
    expect(searchInput.value).toBe("");
    expect(await screen.findByText("subfolder")).toBeInTheDocument();
    expect(screen.queryByText("match-file.txt")).not.toBeInTheDocument();
  });

  it("clears search on Escape key", async () => {
    const { invoke } = await import("@tauri-apps/api/core");
    const mockInvoke = setupMockInvoke();
    vi.mocked(invoke).mockImplementation(mockInvoke);

    render(<App />);
    const spaceCard = (await screen.findAllByText("demo-dataset"))[0];
    fireEvent.click(spaceCard);
    expect(await screen.findByText("subfolder")).toBeInTheDocument();

    const searchInput = screen.getByPlaceholderText("搜索当前空间") as HTMLInputElement;

    // Type and press Enter immediately
    fireEvent.change(searchInput, { target: { value: "match" } });
    fireEvent.keyDown(searchInput, { key: "Enter" });

    expect(await screen.findByText("match-file.txt")).toBeInTheDocument();

    // Press Escape
    fireEvent.keyDown(searchInput, { key: "Escape" });

    expect(searchInput.value).toBe("");
    expect(await screen.findByText("subfolder")).toBeInTheDocument();
    expect(screen.queryByText("match-file.txt")).not.toBeInTheDocument();
  });
});
