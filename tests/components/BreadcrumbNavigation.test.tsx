import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
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

describe("Breadcrumb navigation", () => {
  it("resets query, search results, and selection when navigating via breadcrumbs", async () => {
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
          tree,
          warnings: []
        } as CatalogLoadResult;
      }
      if (cmd === "search_catalog") {
        return [
          {
            id: "search-result-1",
            name: "searched-file.txt",
            kind: "File",
            size: 123,
            updated_at: "2026-01-01T00:00:00Z",
            children_count: 0
          }
        ];
      }
      return null;
    });

    render(<App />);

    // Click on the space card to enter Drive view
    const spaceCard = await screen.findByText("demo-dataset");
    fireEvent.click(spaceCard);

    // Now in Drive view, wait for subfolder to be displayed
    expect(await screen.findByText("subfolder")).toBeInTheDocument();

    // Enter subfolder by clicking the folder button
    fireEvent.click(screen.getByRole("button", { name: "subfolder" }));
    expect(await screen.findByText("此文件夹为空")).toBeInTheDocument();

    // Search something
    const searchInput = screen.getByPlaceholderText("搜索当前空间");
    fireEvent.change(searchInput, { target: { value: "search-term" } });
    fireEvent.keyDown(searchInput, { key: "Enter" });

    expect(await screen.findByText("searched-file.txt")).toBeInTheDocument();

    // Select the search result item checkbox
    const checkboxes = screen.getAllByRole("checkbox");
    // checkboxes[0] is select all, checkboxes[1] is the item
    fireEvent.click(checkboxes[1]);
    expect(await screen.findByText("已选 1 / 1 项")).toBeInTheDocument();

    // Find breadcrumb for demo-dataset (root of space)
    const rootCrumb = screen.getByRole("button", { name: "转到路径：demo-dataset" });
    fireEvent.click(rootCrumb);

    // After clicking breadcrumb, search input should be cleared, search results gone, selection cleared, and subfolder visible again
    await waitFor(() => {
      expect((searchInput as HTMLInputElement).value).toBe("");
    });
    expect(screen.queryByText("searched-file.txt")).not.toBeInTheDocument();
    expect(screen.queryByText(/已选/)).not.toBeInTheDocument();
    expect(screen.getByText("subfolder")).toBeInTheDocument();
  });
});
