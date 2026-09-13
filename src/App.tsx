import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  AlertTriangle,
  ChevronRight,
  Download,
  FolderOpen,
  HardDrive,
  KeyRound,
  Minus,
  RefreshCw,
  Search,
  Settings,
  ShieldCheck,
  Square,
  Trash2,
  UploadCloud,
  X
} from "lucide-react";
import { type MouseEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import liosPetalMark from "./assets/lios-petal-mark.svg";
import type {
  CacheCleanupReport,
  CatalogLoadResult,
  CatalogRebuildDialog,
  CatalogRebuildPreview,
  CatalogTreeNode,
  ConflictAction,
  ConflictResolution,
  DatasetRepoListResult,
  DriveItem,
  ModelScopeUserSummary,
  RecoveryKeyImportDialog,
  Snapshot,
  SpaceSummary,
  UploadConflict
} from "./appTypes.ts";
import {
  createLatestSerialExecutor,
  initializeWithExistingCatalog,
  loadCatalogState
} from "./catalogState.ts";
import { errorText } from "./commandError.ts";
import { AboutSection } from "./features/settings/AboutSection.tsx";
import {
  breadcrumb,
  displayPath,
  findNode,
  formatCacheBytes,
  sameSpace,
  treeToDriveItem
} from "./features/catalog/catalogPresentation.tsx";
import { TaskCenter } from "./features/tasks/TaskCenter.tsx";
import { createTaskApi } from "./features/tasks/taskApi.ts";
import {
  newCatalogMutationCompletions,
  seedCatalogMutationCompletions
} from "./features/tasks/taskPresentation.ts";
import { type TaskSummary } from "./features/tasks/taskTypes.ts";
import { useTasks } from "./features/tasks/useTasks.ts";
import {
  conciseRecoveryKeyPath,
  recoveryKeyBackupText,
  type RecoveryKeyStatus,
  type RecoveryKeyVerification
} from "./recoveryKeyPresentation.ts";
import { setupWarningMessage } from "./setupWarning.ts";

import { ConflictModal } from "./features/drive/ConflictModal.tsx";
import { ContextMenu } from "./features/drive/ContextMenu.tsx";
import { DragDropOverlay } from "./features/drive/DragDropOverlay.tsx";
import { DriveToolbar } from "./features/drive/DriveToolbar.tsx";
import { FileGrid } from "./features/drive/FileGrid.tsx";
import { FileTable } from "./features/drive/FileTable.tsx";
import { FilePreviewModal, type FilePreviewContent } from "./features/drive/FilePreviewModal.tsx";
import type {
  ContextMenuState,
  SortDirection,
  SortField,
  ViewMode
} from "./features/drive/driveTypes.ts";
import { CreateSpaceModal } from "./features/spaces/CreateSpaceModal.tsx";
import { nameToSafeSlug } from "./features/spaces/spaceMapping.ts";
import { SpaceGrid } from "./features/spaces/SpaceGrid.tsx";
import { RebuildCatalogModal } from "./features/catalog/RebuildCatalogModal.tsx";
import { ImportKeyModal } from "./features/recoveryKey/ImportKeyModal.tsx";

type InvokeArgs = Record<string, unknown>;

type TauriRuntimeGlobal = typeof globalThis & {
  isTauri?: boolean;
  __TAURI_INTERNALS__?: {
    invoke?: unknown;
  };
};

function hasTauriRuntime() {
  const runtime = globalThis as TauriRuntimeGlobal;
  return Boolean(runtime.isTauri || runtime.__TAURI_INTERNALS__?.invoke);
}

function previewTree(): CatalogTreeNode {
  return {
    id: "root-dir",
    name: "",
    updated_at: "2026-09-12T10:00:00Z",
    kind: {
      type: "Directory",
      children: [
        {
          id: "dir-docs",
          name: "Documents",
          updated_at: "2026-09-10T14:30:00Z",
          kind: {
            type: "Directory",
            children: [
              {
                id: "file-spec",
                name: "architecture_spec.pdf",
                updated_at: "2026-09-10T14:28:00Z",
                kind: {
                  type: "File",
                  original_size: 2457600,
                  sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                  object_id: "obj-1",
                  chunk_count: 2
                }
              },
              {
                id: "file-notes",
                name: "release_notes.md",
                updated_at: "2026-09-09T18:12:00Z",
                kind: {
                  type: "File",
                  original_size: 14280,
                  sha256: "d41d8cd98f00b204e9800998ecf8427e",
                  object_id: "obj-2",
                  chunk_count: 1
                }
              }
            ]
          }
        },
        {
          id: "dir-photos",
          name: "Photos",
          updated_at: "2026-09-08T09:15:00Z",
          kind: {
            type: "Directory",
            children: [
              {
                id: "file-img1",
                name: "sunset_lake.png",
                updated_at: "2026-09-08T09:10:00Z",
                kind: {
                  type: "File",
                  original_size: 4892100,
                  sha256: "abc123",
                  object_id: "obj-3",
                  chunk_count: 4
                }
              },
              {
                id: "file-img2",
                name: "mountain_peak.jpg",
                updated_at: "2026-09-07T16:45:00Z",
                kind: {
                  type: "File",
                  original_size: 3200150,
                  sha256: "def456",
                  object_id: "obj-4",
                  chunk_count: 3
                }
              }
            ]
          }
        },
        {
          id: "file-data",
          name: "dataset_manifest.json",
          updated_at: "2026-09-11T20:00:00Z",
          kind: {
            type: "File",
            original_size: 89420,
            sha256: "789ghi",
            object_id: "obj-5",
            chunk_count: 1
          }
        },
        {
          id: "file-archive",
          name: "training_checkpoint.tar.gz",
          updated_at: "2026-09-05T11:20:00Z",
          kind: {
            type: "File",
            original_size: 145892000,
            sha256: "jkl012",
            object_id: "obj-6",
            chunk_count: 12
          }
        }
      ]
    }
  };
}

function previewSnapshot(): Snapshot {
  return {
    paths: {
      home: "~/.lios",
      config: "~/.lios/config.yaml",
      database: "~/.lios/lios.db",
      staging: "~/.lios/staging",
      logs: "~/.lios/logs",
      credentials: "~/.lios/credentials.enc"
    },
    config: {
      schema_version: 2,
      spaces: {
        photos: {
          namespace: "lingbou",
          dataset: "photos",
          endpoint: "https://modelscope.cn"
        },
        research_data: {
          namespace: "lingbou",
          dataset: "research_data",
          endpoint: "https://modelscope.cn"
        },
        checkpoints: {
          namespace: "lingbou",
          dataset: "checkpoints",
          endpoint: "https://modelscope.cn"
        }
      },
      key_file_path: "~/.lios/recovery.key",
      chunk_size: 134217728
    },
    recovery_key: {
      key_location: "~/.lios/recovery.key",
      backed_up: true,
      backup_location: "~/Documents/lios-recovery.key"
    },
    has_token: true,
    spaces: [
      {
        space_name: "photos",
        namespace: "lingbou",
        dataset: "photos",
        endpoint: "https://modelscope.cn",
        visibility: "Private",
        updated_at: "2026-09-12T10:00:00Z",
        task_space_id: "task-space-1"
      },
      {
        space_name: "research_data",
        namespace: "lingbou",
        dataset: "research_data",
        endpoint: "https://modelscope.cn",
        visibility: "Private",
        updated_at: "2026-09-11T16:00:00Z",
        task_space_id: "task-space-2"
      },
      {
        space_name: "checkpoints",
        namespace: "lingbou",
        dataset: "checkpoints",
        endpoint: "https://modelscope.cn",
        visibility: "Private",
        updated_at: "2026-09-08T12:00:00Z",
        task_space_id: "task-space-3"
      }
    ],
    warning: null
  };
}

async function loadAppVersion() {
  if (!hasTauriRuntime()) return "开发预览";
  return getVersion();
}

async function appInvoke<T>(command: string, args?: InvokeArgs): Promise<T> {
  if (hasTauriRuntime()) return invoke<T>(command, args);
  if (command === "current_setup") return previewSnapshot() as T;
  if (command === "list_tasks") return [] as T;
  if (command === "list_task_items") {
    return { task_id: args?.taskId, offset: args?.offset ?? 0, total: 0, items: [] } as T;
  }
  if (command === "list_dataset_repos") {
    return {
      user: { username: "lingbou", email: "lingbou@modelscope.cn" },
      repositories: previewSnapshot().spaces
    } as T;
  }
  if (command === "load_space_catalog") {
    return {
      local_path: "~/.lios/staging",
      bytes: 156480000,
      tree: previewTree(),
      warnings: []
    } as T;
  }
  if (command === "search_catalog") {
    const q = ((args?.query as string) || "").toLowerCase();
    const tree = previewTree();
    const all: DriveItem[] = tree.kind.type === "Directory" ? tree.kind.children.map(treeToDriveItem) : [];
    return all.filter((item: DriveItem) => item.name.toLowerCase().includes(q)) as T;
  }
  if (command === "cleanup_local_cache") {
    return { files_removed: 4, dirs_removed: 2, bytes_removed: 20971520 } as T;
  }
  if (command === "register_space") {
    return null as T;
  }
  if (command === "set_chunk_size") {
    return null as T;
  }
  if (command === "remove_space") {
    return null as T;
  }
  if (command === "preview_file_node") {
    return {
      name: "document.md",
      size: 1024,
      mime_type: "text/markdown",
      is_text: true,
      text: "# Lios 存储与加密架构\n\n- 端到端零知识加密\n- 自适应高熵压缩旁路 (Zstd Level 1)\n- 自动空间发现与无感绑定",
      data_url: undefined
    } as T;
  }
  if (command === "create_folder") {
    return { local_path: "", bytes: 0, tree: previewTree(), warnings: [] } as T;
  }
  if (command === "rename_node") {
    return { local_path: "", bytes: 0, tree: previewTree(), warnings: [] } as T;
  }
  throw new Error("这个操作需要在 Lios 桌面端中执行");
}

const taskApi = createTaskApi(appInvoke);

type View = "spaces" | "drive" | "settings";
type CatalogStatus = "idle" | "loading" | "ready" | "missing" | "error";

function App() {
  const [view, setView] = useState<View>("spaces");
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [spaces, setSpaces] = useState<SpaceSummary[]>([]);
  const [spacesLoaded, setSpacesLoaded] = useState(false);
  const [modelscopeUser, setModelscopeUser] = useState<ModelScopeUserSummary | null>(null);
  const [activeSpace, setActiveSpace] = useState<SpaceSummary | null>(null);
  const [catalogTree, setCatalogTree] = useState<CatalogTreeNode | null>(null);
  const [catalogStatus, setCatalogStatus] = useState<CatalogStatus>("idle");
  const [currentFolderId, setCurrentFolderId] = useState<string | null>(null);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [lastSelectedId, setLastSelectedId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [searchResults, setSearchResults] = useState<DriveItem[]>([]);
  const [token, setToken] = useState("");
  const [manualEndpoint, setManualEndpoint] = useState("https://modelscope.cn");
  const [createSpaceOpen, setCreateSpaceOpen] = useState(false);
  const [newSpaceName, setNewSpaceName] = useState("");
  const [createSpaceError, setCreateSpaceError] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const handleTaskError = useCallback((error: unknown) => setMessage(errorText(error)), []);
  const {
    tasks,
    ready: tasksReady,
    pendingActions,
    onAction: runTaskAction,
    listTaskItems,
    refreshTasks,
    upsertTask
  } = useTasks({ api: taskApi, onError: handleTaskError });
  const [pendingUpload, setPendingUpload] = useState<string[]>([]);
  const [conflicts, setConflicts] = useState<UploadConflict[]>([]);
  const [conflictActions, setConflictActions] = useState<Record<string, ConflictAction>>({});
  const [cacheCleanup, setCacheCleanup] = useState<CacheCleanupReport | null>(null);
  const [rebuildDialog, setRebuildDialog] = useState<CatalogRebuildDialog | null>(null);
  const [recoveryKeyImport, setRecoveryKeyImport] = useState<RecoveryKeyImportDialog | null>(null);
  const catalogLoads = useRef(createLatestSerialExecutor()).current;
  const catalogMutationCompletions = useRef<Set<string>>(new Set());
  const catalogMutationCompletionBaselineReady = useRef(false);
  const rebuildPreviewRequest = useRef(0);

  // New states for enhanced interaction
  const [sortField, setSortField] = useState<SortField>("name");
  const [sortDirection, setSortDirection] = useState<SortDirection>("asc");
  const [viewMode, setViewMode] = useState<ViewMode>(() => {
    try {
      const stored = globalThis.localStorage?.getItem("lios.viewMode");
      if (stored === "grid" || stored === "table") return stored;
    } catch {
      // Fall back
    }
    return "table";
  });
  const [isDragging, setIsDragging] = useState(false);
  const [contextMenu, setContextMenu] = useState<ContextMenuState>({
    open: false,
    x: 0,
    y: 0,
    item: null
  });

  // In-app file preview states
  const [previewOpen, setPreviewOpen] = useState(false);
  const [previewItem, setPreviewItem] = useState<DriveItem | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewContent, setPreviewContent] = useState<FilePreviewContent | null>(null);



  const currentFolder = findNode(catalogTree, currentFolderId);
  const children =
    currentFolder?.kind.type === "Directory" ? currentFolder.kind.children.map(treeToDriveItem) : [];
  const visibleItems = query.trim() ? searchResults : children;

  const sortedItems = useMemo(() => {
    const items = [...visibleItems];
    items.sort((a, b) => {
      if (sortField !== "kind") {
        if (a.kind === "Directory" && b.kind === "File") return -1;
        if (a.kind === "File" && b.kind === "Directory") return 1;
      }

      let result = 0;
      if (sortField === "name") {
        result = a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
      } else if (sortField === "kind") {
        if (a.kind === b.kind) {
          result = a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
        } else {
          result = a.kind === "Directory" ? -1 : 1;
        }
      } else if (sortField === "size") {
        const sizeA = a.kind === "File" ? a.size : 0;
        const sizeB = b.kind === "File" ? b.size : 0;
        result = sizeA - sizeB;
      } else if (sortField === "updated_at") {
        result = new Date(a.updated_at).getTime() - new Date(b.updated_at).getTime();
      }

      return sortDirection === "asc" ? result : -result;
    });
    return items;
  }, [visibleItems, sortField, sortDirection]);

  const previewableFiles = useMemo(
    () => sortedItems.filter((i) => i.kind === "File"),
    [sortedItems]
  );
  const previewIndex = previewItem
    ? previewableFiles.findIndex((i) => i.id === previewItem.id)
    : -1;
  const hasPrevPreview = previewIndex > 0;
  const hasNextPreview = previewIndex >= 0 && previewIndex < previewableFiles.length - 1;
  const handlePrevPreview = () => {
    if (hasPrevPreview) void openFilePreview(previewableFiles[previewIndex - 1]);
  };
  const handleNextPreview = () => {
    if (hasNextPreview) void openFilePreview(previewableFiles[previewIndex + 1]);
  };

  const rawCrumbs = breadcrumb(catalogTree, currentFolderId);
  const crumbs = rawCrumbs.map((crumb, index) =>
    index === 0 && !crumb.name ? { ...crumb, name: (activeSpace?.title || activeSpace?.dataset) ?? "根目录" } : crumb
  );
  const crumbPaths = crumbs.map((_crumb, index) =>
    crumbs
      .slice(0, index + 1)
      .map((crumb) => crumb.name)
      .join(" / ")
  );
  const selectedCount = selectedIds.size;
  const activeTasks = tasks.filter((task) =>
    ["Queued", "Preparing", "Running", "Paused", "Retrying", "Committing"].includes(task.state)
  ).length;
  const rebuildTaskActive = tasks.some(
    (task) =>
      task.label === "rebuild" &&
      ["Queued", "Preparing", "Running", "Paused", "Retrying", "Committing"].includes(task.state)
  );
  const hasToken = Boolean(snapshot?.has_token);
  const selectedSpace = activeSpace;

  const displayedSpaces = spaces;
  const visibleSpaces = query.trim()
    ? displayedSpaces.filter((space) =>
        `${space.dataset} ${space.namespace}`.toLowerCase().includes(query.trim().toLowerCase())
      )
    : displayedSpaces;
  const hasSpaces = displayedSpaces.length > 0;
  const emptyDriveMode = !hasToken ? "connect" : hasSpaces ? "select" : "create";
  const accountName = modelscopeUser?.username ?? "未连接账号";
  const crumbFallbackLabel =
    view === "spaces"
      ? accountName
      : activeSpace
        ? (activeSpace.title || activeSpace.dataset)
        : emptyDriveMode === "create"
          ? "创建一个空间"
          : emptyDriveMode === "connect"
            ? "连接 ModelScope"
            : "选择一个空间";
  const fullBreadcrumbPath = crumbPaths[crumbPaths.length - 1] ?? crumbFallbackLabel;

  async function minimizeWindow() {
    if (!hasTauriRuntime()) return;
    try {
      await getCurrentWindow().minimize();
    } catch (error) {
      setMessage(errorText(error));
    }
  }

  async function toggleMaximizeWindow() {
    if (!hasTauriRuntime()) return;
    try {
      await getCurrentWindow().toggleMaximize();
    } catch (error) {
      setMessage(errorText(error));
    }
  }

  async function closeWindow() {
    if (!hasTauriRuntime()) return;
    try {
      await getCurrentWindow().close();
    } catch (error) {
      setMessage(errorText(error));
    }
  }

  async function startWindowDrag(event: MouseEvent<HTMLElement>) {
    if (event.button !== 0 || !hasTauriRuntime()) return;
    event.preventDefault();
    try {
      await getCurrentWindow().startDragging();
    } catch (error) {
      setMessage(errorText(error));
    }
  }

  async function refreshSetup(loadSpaces = true) {
    const next = await appInvoke<Snapshot>("current_setup");
    setSnapshot(next);
    const warning = setupWarningMessage(next.warning);
    if (warning) setMessage(warning);
    setSpaces(next.spaces);
    setSpacesLoaded(true);
    const preferredName =
      activeSpace?.space_name ?? globalThis.localStorage?.getItem("lios.lastSpaceName") ?? null;
    const visibleActiveRepo = preferredName
      ? next.spaces.find((space) => space.space_name === preferredName) ?? null
      : null;
    setManualEndpoint((current) => visibleActiveRepo?.endpoint || current);
    if (loadSpaces && next.has_token) {
      const result = await appInvoke<DatasetRepoListResult>("list_dataset_repos", {
        endpoint: visibleActiveRepo?.endpoint || manualEndpoint
      });
      setModelscopeUser(result.user);

      // Auto-prune any locally registered space whose remote repo has been deleted on ModelScope
      const remoteRepoSet = new Set(result.repositories.map((r) => `${r.namespace}/${r.dataset}`));
      let anyPruned = false;
      for (const space of next.spaces) {
        if (
          space.namespace === result.user.username &&
          !remoteRepoSet.has(`${space.namespace}/${space.dataset}`)
        ) {
          try {
            await appInvoke("remove_space", { name: space.space_name });
            anyPruned = true;
          } catch {
            // Ignore
          }
        }
      }

      // Auto-register any discovered ASCII ModelScope dataset into local space registry
      const registeredSet = new Set(next.spaces.map((s) => `${s.namespace}/${s.dataset}`));
      let anyRegistered = false;
      for (const repo of result.repositories) {
        if (!registeredSet.has(`${repo.namespace}/${repo.dataset}`)) {
          const defaultAlias = repo.dataset.toLowerCase();
          if (/^[a-z][a-z0-9_-]{0,31}$/.test(defaultAlias)) {
            try {
              await appInvoke("register_space", {
                name: defaultAlias,
                namespace: repo.namespace,
                dataset: repo.dataset,
                endpoint: repo.endpoint
              });
              anyRegistered = true;
            } catch {
              // Ignore alias collision or format error
            }
          }
        }
      }
      if (anyRegistered || anyPruned) {
        const updatedSetup = await appInvoke<Snapshot>("current_setup");
        setSnapshot(updatedSetup);
        setSpaces(updatedSetup.spaces);
        const updatedActive = preferredName
          ? updatedSetup.spaces.find((space) => space.space_name === preferredName) ?? null
          : null;
        if (updatedActive) {
          setActiveSpace(updatedActive);
        } else if (activeSpace && !updatedSetup.spaces.some((s) => s.space_name === activeSpace.space_name)) {
          setActiveSpace(null);
          setCatalogTree(null);
          setCatalogStatus("idle");
        }
        return updatedActive;
      }
    }

    if (visibleActiveRepo) {
      setActiveSpace(visibleActiveRepo);
    } else if (loadSpaces || spacesLoaded) {
      setActiveSpace(null);
      globalThis.localStorage?.removeItem("lios.lastSpaceName");
      setCatalogTree(null);
      setCatalogStatus("idle");
      setCurrentFolderId(null);
    }
    return visibleActiveRepo;
  }

  async function run(label: string, action: () => Promise<unknown>) {
    setBusy(label);
    setMessage("");
    try {
      await action();
      await refreshSetup(false);
    } catch (error) {
      setMessage(errorText(error));
      await refreshSetup(false).catch(() => undefined);
      await refreshTasks().catch(() => undefined);
    } finally {
      setBusy(null);
    }
  }

  useEffect(() => {
    refreshSetup().catch((error) => setMessage(errorText(error)));
  }, []);

  useEffect(() => {
    if (!tasksReady) return;
    if (!catalogMutationCompletionBaselineReady.current) {
      seedCatalogMutationCompletions(catalogMutationCompletions.current, tasks);
      catalogMutationCompletionBaselineReady.current = true;
      return;
    }
    if (!activeSpace?.task_space_id) return;

    const completedMutations = newCatalogMutationCompletions(
      catalogMutationCompletions.current,
      tasks,
      activeSpace.task_space_id
    );
    if (completedMutations.length > 0) {
      reloadCatalog(false, activeSpace).catch((error) => setMessage(errorText(error)));
    }
  }, [tasks, tasksReady, activeSpace?.task_space_id]);

  useEffect(() => {
    if (view === "drive" && activeSpace && catalogStatus === "idle") {
      void loadSpace(activeSpace);
    }
  }, [view, activeSpace, catalogStatus]);

  // Native Tauri drag and drop integration
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    if (hasTauriRuntime()) {
      try {
        getCurrentWebview()
          .onDragDropEvent((event) => {
            if (event.payload.type === "enter" || event.payload.type === "over") {
              if (view === "drive" && activeSpace && currentFolderId) {
                setIsDragging(true);
              }
            } else if (event.payload.type === "leave") {
              setIsDragging(false);
            } else if (event.payload.type === "drop") {
              setIsDragging(false);
              if (view === "drive" && activeSpace && currentFolderId) {
                const paths = event.payload.paths;
                if (paths && paths.length > 0) {
                  void queueUpload(paths);
                }
              }
            }
          })
          .then((cleanup) => {
            unlisten = cleanup;
          })
          .catch(() => undefined);
      } catch {
        // Fall back gracefully
      }
    }
    return () => {
      if (unlisten) unlisten();
    };
  }, [view, activeSpace, currentFolderId]);

  useEffect(() => {
    closeContextMenu();
  }, [view, currentFolderId]);

  // Global keyboard shortcuts
  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      const activeTag = document.activeElement?.tagName?.toLowerCase();
      if (activeTag === "input" || activeTag === "select" || activeTag === "textarea") {
        return;
      }

      if (view === "drive" && activeSpace && catalogTree) {
        if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "a") {
          event.preventDefault();
          selectAll();
          return;
        }
        if (event.key === "Delete" || event.key === "Backspace") {
          if (selectedIds.size > 0 && busy === null) {
            event.preventDefault();
            void deleteSelected();
          }
          return;
        }
        if (event.key === "F2") {
          if (selectedIds.size === 1 && busy === null) {
            event.preventDefault();
            void renameSelected();
          }
          return;
        }
        if (event.key === "Enter") {
          if (selectedIds.size === 1) {
            const [selectedId] = [...selectedIds];
            const target = sortedItems.find((item) => item.id === selectedId);
            if (target && target.kind === "Directory") {
              event.preventDefault();
              enterItem(target);
            }
          }
          return;
        }
        if (event.key === "Escape") {
          if (selectedIds.size > 0) {
            event.preventDefault();
            setSelectedIds(new Set());
          }
          return;
        }
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [view, activeSpace, catalogTree, selectedIds, sortedItems, busy]);

  async function loadSpace(space: SpaceSummary) {
    setView("drive");
    setActiveSpace(space);
    globalThis.localStorage?.setItem("lios.lastSpaceName", space.space_name);
    setCatalogStatus("loading");
    setCatalogTree(null);
    setCurrentFolderId(null);
    setSelectedIds(new Set());
    setLastSelectedId(null);
    setSearchResults([]);
    setQuery("");
    await catalogLoads.run(async (request) => {
      try {
        const outcome = await loadCatalogState(() =>
          appInvoke<CatalogLoadResult>("load_space_catalog", { spaceName: space.space_name })
        );
        if (!request.isCurrent()) return;
        if (outcome.status === "missing") {
          setCatalogStatus("missing");
          setMessage("");
          return;
        }
        const result = outcome.catalog;
        setCatalogTree(result.tree);
        setCurrentFolderId(result.tree.id);
        setCatalogStatus("ready");
        setMessage(result.warnings.join("; "));
      } catch (error) {
        if (!request.isCurrent()) return;
        setCatalogTree(null);
        setCurrentFolderId(null);
        setCatalogStatus("error");
        setMessage(errorText(error));
      }
    });
  }

  async function initializeActiveSpace() {
    if (!activeSpace) return;
    setBusy("初始化空间");
    setMessage("");
    try {
      await initializeWithExistingCatalog(
        async () => {
          const result = await appInvoke<CatalogLoadResult>("initialize_space", {
            spaceName: activeSpace.space_name
          });
          setCatalogTree(result.tree);
          setCurrentFolderId(result.tree.id);
          setCatalogStatus("ready");
          setSelectedIds(new Set());
          setLastSelectedId(null);
          setMessage(result.warnings.join("; "));
        },
        () => reloadCatalog(true)
      );
      await refreshSetup(false);
    } catch (error) {
      setMessage(errorText(error));
    } finally {
      setBusy(null);
    }
  }

  async function reloadCatalog(rethrow = false, targetSpace = activeSpace) {
    if (!targetSpace) return;
    setCatalogStatus("loading");
    setMessage("");
    return catalogLoads.run(async (request) => {
      try {
        const outcome = await loadCatalogState(() =>
          appInvoke<CatalogLoadResult>("load_space_catalog", {
            spaceName: targetSpace.space_name
          })
        );
        if (!request.isCurrent()) return;
        if (outcome.status === "missing") {
          setCatalogTree(null);
          setCurrentFolderId(null);
          setCatalogStatus("missing");
          return;
        }
        const result = outcome.catalog;
        setCatalogTree(result.tree);
        if (!currentFolderId) setCurrentFolderId(result.tree.id);
        setCatalogStatus("ready");
        setSelectedIds(new Set());
        setLastSelectedId(null);
        setMessage(result.warnings.join("; "));
        const trimmedQuery = query.trim();
        if (trimmedQuery) {
          const results = await appInvoke<DriveItem[]>("search_catalog", {
            spaceName: targetSpace.space_name,
            query: trimmedQuery
          });
          if (!request.isCurrent()) return;
          setSearchResults(results);
        }
      } catch (error) {
        if (!request.isCurrent()) return;
        setCatalogTree(null);
        setCurrentFolderId(null);
        setCatalogStatus("error");
        setMessage(errorText(error));
        if (rethrow) throw error;
      }
    });
  }

  function toggleSelection(id: string) {
    setLastSelectedId(id);
    setSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function selectOnly(id: string) {
    setLastSelectedId(id);
    setSelectedIds(new Set([id]));
  }

  function selectRange(targetId: string) {
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
  }

  function selectAll() {
    if (sortedItems.length === 0) return;
    if (sortedItems.every((item) => selectedIds.has(item.id))) {
      setSelectedIds(new Set());
    } else {
      setSelectedIds(new Set(sortedItems.map((item) => item.id)));
    }
  }

  async function openFilePreview(item: DriveItem) {
    if (!activeSpace) return;
    setPreviewItem(item);
    setPreviewOpen(true);
    setPreviewLoading(true);
    setPreviewError(null);
    setPreviewContent(null);
    try {
      const res = await appInvoke<{
        name: string;
        size: number;
        mime_type: string;
        is_text: boolean;
        text?: string;
        data_url?: string;
      }>("preview_file_node", {
        spaceName: activeSpace.space_name,
        nodeId: item.id
      });
      setPreviewContent({
        isText: res.is_text,
        text: res.text,
        dataUrl: res.data_url
      });
    } catch (err) {
      setPreviewError(errorText(err));
    } finally {
      setPreviewLoading(false);
    }
  }

  function enterItem(item: DriveItem) {
    if (item.kind === "Directory") {
      setCurrentFolderId(item.id);
      setSelectedIds(new Set());
      setLastSelectedId(null);
      setQuery("");
      setSearchResults([]);
    } else {
      toggleSelection(item.id);
      void openFilePreview(item);
    }
  }

  function handleSort(field: SortField) {
    if (sortField === field) {
      setSortDirection((current) => (current === "asc" ? "desc" : "asc"));
    } else {
      setSortField(field);
      setSortDirection("asc");
    }
  }

  function handleViewModeChange(mode: ViewMode) {
    setViewMode(mode);
    try {
      globalThis.localStorage?.setItem("lios.viewMode", mode);
    } catch {
      // ignore
    }
  }

  function handleContextMenu(event: MouseEvent, item: DriveItem | null) {
    setContextMenu({
      open: true,
      x: event.clientX,
      y: event.clientY,
      item
    });
  }

  function closeContextMenu() {
    setContextMenu((current) => (current.open ? { ...current, open: false } : current));
  }

  async function queueUpload(paths: string[]) {
    if (paths.length === 0 || !currentFolderId || !activeSpace) return;
    const found = await appInvoke<UploadConflict[]>("preview_upload_conflicts", {
      spaceName: activeSpace.space_name,
      parentNodeId: currentFolderId,
      paths
    });
    if (found.length > 0) {
      setPendingUpload(paths);
      setConflicts(found);
      setConflictActions(
        Object.fromEntries(found.map((conflict) => [conflict.source_path, "KeepBoth"]))
      );
      return;
    }
    await startUpload(paths, []);
  }

  async function pickUpload(directory: boolean) {
    if (!activeSpace) return;
    const selected = await open({ directory, multiple: !directory });
    const paths = Array.isArray(selected)
      ? selected.filter((item): item is string => typeof item === "string")
      : typeof selected === "string"
        ? [selected]
        : [];
    await queueUpload(paths);
  }

  async function startUpload(paths: string[], resolutions: ConflictResolution[]) {
    if (!currentFolderId || !activeSpace) return;
    await run("上传", async () => {
      const task = await appInvoke<TaskSummary>("enqueue_upload_to_folder", {
        spaceName: activeSpace.space_name,
        parentNodeId: currentFolderId,
        paths,
        conflictResolutions: resolutions
      });
      upsertTask(task);
    });
  }

  async function confirmConflicts() {
    const resolutions = conflicts.map((conflict) => ({
      source_path: conflict.source_path,
      action: conflictActions[conflict.source_path] || "KeepBoth"
    }));
    const paths = pendingUpload;
    setPendingUpload([]);
    setConflicts([]);
    setConflictActions({});
    await startUpload(paths, resolutions);
  }

  function handleConflictAction(sourcePath: string, action: ConflictAction) {
    setConflictActions((current) => ({
      ...current,
      [sourcePath]: action
    }));
  }

  function handleSetAllConflictActions(action: ConflictAction) {
    setConflictActions(
      Object.fromEntries(conflicts.map((conflict) => [conflict.source_path, action]))
    );
  }

  async function createFolder() {
    if (!currentFolderId || !activeSpace) return;
    const name = window.prompt("文件夹名称");
    if (!name) return;
    await run("新建文件夹", async () => {
      const result = await appInvoke<CatalogLoadResult>("create_folder", {
        spaceName: activeSpace.space_name,
        parentNodeId: currentFolderId,
        name
      });
      setCatalogTree(result.tree);
      setCatalogStatus("ready");
      setMessage(result.warnings.join("; "));
    });
  }

  async function renameSelected() {
    if (!activeSpace) return;
    const [nodeId] = [...selectedIds];
    if (!nodeId) return;
    const node = findNode(catalogTree, nodeId);
    const newName = window.prompt("新名称", node?.name ?? "");
    if (!newName) return;
    await run("重命名", async () => {
      const result = await appInvoke<CatalogLoadResult>("rename_node", {
        spaceName: activeSpace.space_name,
        nodeId,
        newName
      });
      setCatalogTree(result.tree);
      setCatalogStatus("ready");
      setSelectedIds(new Set());
      setLastSelectedId(null);
      setMessage(result.warnings.join("; "));
    });
  }

  async function deleteSelected() {
    if (selectedIds.size === 0 || !activeSpace) return;
    const ok = window.confirm(
      `从 Lios 目录中删除 ${selectedIds.size} 个项目？此操作不会进入回收站。\n\nModelScope 的令牌接口目前不支持物理删除远端文件；如需释放远端空间，请在 ModelScope 网页端删除或重建这个空间。`
    );
    if (!ok) return;
    const nodeIds = [...selectedIds];
    await run("删除", async () => {
      const task = await appInvoke<TaskSummary>("enqueue_delete_nodes", {
        spaceName: activeSpace.space_name,
        nodeIds
      });
      upsertTask(task);
    });
  }

  async function downloadSelected() {
    if (!activeSpace) return;
    const nodeIds = [...selectedIds];
    if (nodeIds.length === 0) return;
    const output = await open({ directory: true, multiple: false });
    if (typeof output !== "string") return;
    await run("下载", async () => {
      const task = await appInvoke<TaskSummary>("enqueue_download", {
        spaceName: activeSpace.space_name,
        nodeIds,
        outputDir: output
      });
      upsertTask(task);
    });
  }

  async function searchCatalog(value = query) {
    const trimmed = value.trim();
    setQuery(value);
    if (!trimmed) {
      setSearchResults([]);
      return;
    }
    if (!activeSpace) return;
    const results = await appInvoke<DriveItem[]>("search_catalog", {
      spaceName: activeSpace.space_name,
      query: trimmed
    });
    setSearchResults(results);
  }

  async function saveToken() {
    await run("连接账号", async () => {
      await appInvoke("setup_token", { token });
      setToken("");
      await refreshSetup(true);
    });
  }

  async function cleanupLocalCache() {
    await run("清理本地缓存", async () => {
      const result = await appInvoke<CacheCleanupReport>("cleanup_local_cache");
      setCacheCleanup(result);
    });
  }

  async function exportRecoveryKey() {
    const destination = await save({
      defaultPath: "lios-recovery.key",
      filters: [{ name: "Lios recovery key", extensions: ["key", "yaml", "yml"] }]
    });
    if (typeof destination !== "string") return;
    await run("导出恢复密钥", async () => {
      await appInvoke<RecoveryKeyStatus>("export_recovery_key", { destination });
      setMessage("恢复密钥备份已导出。请将它保存在独立且安全的位置。");
    });
  }

  async function selectRecoveryKeyForImport() {
    const selected = await open({
      directory: false,
      multiple: false,
      filters: [{ name: "Lios recovery key", extensions: ["key", "yaml", "yml"] }]
    });
    if (typeof selected !== "string") return;
    setBusy("验证恢复密钥");
    setMessage("");
    try {
      const verification = await appInvoke<RecoveryKeyVerification>("verify_recovery_key", {
        path: selected
      });
      setRecoveryKeyImport({ path: selected, verification, importing: false, error: "" });
    } catch (error) {
      setMessage(errorText(error));
    } finally {
      setBusy(null);
    }
  }

  async function refreshVerifiedCatalog(space: SpaceSummary) {
    return catalogLoads.run(async (request) => {
      try {
        const outcome = await loadCatalogState(() =>
          appInvoke<CatalogLoadResult>("load_space_catalog", { spaceName: space.space_name })
        );
        if (!request.isCurrent()) return;
        if (outcome.status === "missing") {
          setCatalogTree(null);
          setCurrentFolderId(null);
          setCatalogStatus("missing");
          return;
        }
        const result = outcome.catalog;
        setCatalogTree(result.tree);
        setCurrentFolderId(result.tree.id);
        setCatalogStatus("ready");
        setSelectedIds(new Set());
        setLastSelectedId(null);
        setSearchResults([]);
        setQuery("");
      } catch (error) {
        if (!request.isCurrent()) return;
        throw error;
      }
    });
  }

  async function confirmRecoveryKeyImport() {
    const dialog = recoveryKeyImport;
    if (!dialog || dialog.importing) return;
    setBusy("导入恢复密钥");
    setRecoveryKeyImport({ ...dialog, importing: true, error: "" });
    let verification: RecoveryKeyVerification;
    try {
      verification = await appInvoke<RecoveryKeyVerification>("import_recovery_key", {
        path: dialog.path
      });
    } catch (error) {
      setRecoveryKeyImport((current) =>
        current ? { ...current, importing: false, error: errorText(error) } : current
      );
      setBusy(null);
      return;
    }

    setRecoveryKeyImport(null);
    try {
      await refreshSetup(false);
      if (
        verification.catalog_checked &&
        verification.checked_space &&
        selectedSpace &&
        sameSpace(selectedSpace, verification.checked_space)
      ) {
        await refreshVerifiedCatalog(selectedSpace);
        setMessage("恢复密钥已导入，并已刷新验证过的空间。");
      } else {
        setMessage("恢复密钥已导入。外部密钥文件必须保持可用。");
      }
    } catch (error) {
      setMessage(`恢复密钥已导入，但刷新状态失败：${errorText(error)}`);
    } finally {
      setBusy(null);
    }
  }

  async function verifySpace(full: boolean) {
    if (!selectedSpace) {
      setMessage("先从账号中选择一个空间。");
      return;
    }
    await run(full ? "完整检查" : "快速检查", async () => {
      const task = await appInvoke<TaskSummary>("enqueue_verify_space", {
        spaceName: selectedSpace.space_name,
        full
      });
      upsertTask(task);
    });
  }

  async function loadCatalogRebuildPreview(space: SpaceSummary) {
    const requestId = rebuildPreviewRequest.current + 1;
    rebuildPreviewRequest.current = requestId;
    setMessage("");
    setRebuildDialog({ space, status: "loading", preview: null, error: "" });
    try {
      const preview = await appInvoke<CatalogRebuildPreview>("preview_rebuild_catalog", {
        spaceName: space.space_name
      });
      if (rebuildPreviewRequest.current !== requestId) return;
      setRebuildDialog({ space, status: "ready", preview, error: "" });
    } catch (error) {
      if (rebuildPreviewRequest.current !== requestId) return;
      setRebuildDialog({
        space,
        status: "error",
        preview: null,
        error: errorText(error)
      });
    }
  }

  async function openCatalogRebuildDialog() {
    if (!selectedSpace) {
      setMessage("先从账号中选择一个空间。");
      return;
    }
    if (rebuildTaskActive) {
      setMessage("当前已有目录恢复任务，请等待它完成后再试。");
      return;
    }
    if (busy !== null || rebuildDialog) return;
    await loadCatalogRebuildPreview(selectedSpace);
  }

  function closeCatalogRebuildDialog() {
    if (rebuildDialog?.status === "submitting") return;
    rebuildPreviewRequest.current += 1;
    setRebuildDialog(null);
  }

  async function confirmCatalogRebuild() {
    const dialog = rebuildDialog;
    if (!dialog?.preview || dialog.status === "loading" || dialog.status === "submitting") return;
    if (rebuildTaskActive) {
      setRebuildDialog({
        ...dialog,
        status: "error",
        error: "当前已有目录恢复任务，请等待它完成后重新预览。"
      });
      return;
    }

    const preview = dialog.preview;
    setRebuildDialog({ ...dialog, status: "submitting", error: "" });
    let task: TaskSummary;
    try {
      task = await appInvoke<TaskSummary>("enqueue_rebuild_catalog", {
        spaceName: dialog.space.space_name,
        expectedRevision: preview.revision
      });
    } catch (error) {
      setRebuildDialog((current) =>
        current
          ? {
              ...current,
              status: "error",
              preview,
              error: errorText(error)
            }
          : current
      );
      await refreshTasks().catch(() => undefined);
      return;
    }

    upsertTask(task);
    setRebuildDialog(null);
  }

  async function handleRemoveSpace(space: SpaceSummary) {
    const ok = window.confirm(
      `确定从本地移除空间「${space.dataset}」？此操作仅移除本地空间别名映射，不会影响远端数据。`
    );
    if (!ok) return;
    try {
      await appInvoke("remove_space", { name: space.space_name });
      if (activeSpace?.space_name === space.space_name) {
        setActiveSpace(null);
        setCatalogTree(null);
        setCatalogStatus("idle");
      }
      await refreshSetup(false);
    } catch (error) {
      setMessage(errorText(error));
    }
  }

  async function selectAccount() {
    setView("spaces");
    setMessage("");
    setQuery("");
    if (!hasToken) return;
    try {
      await refreshSetup(true);
    } catch (error) {
      setMessage(errorText(error));
    }
  }

  function openCreateSpaceDialog() {
    if (!hasToken) {
      setView("settings");
      return;
    }
    if (!modelscopeUser?.username) {
      setMessage("先连接账号，再创建空间。");
      setView("settings");
      return;
    }
    setCreateSpaceError("");
    setNewSpaceName("");
    setCreateSpaceOpen(true);
  }

  async function submitCreateSpace() {
    if (!modelscopeUser?.username) return;
    const nameTrimmed = newSpaceName.trim();
    if (!nameTrimmed) {
      setCreateSpaceError("请输入空间名称");
      return;
    }
    const repoId = nameToSafeSlug(nameTrimmed);
    setBusy("创建空间");
    setMessage("");
    setCreateSpaceError("");
    try {
      await appInvoke("create_dataset_repo", {
        name: repoId,
        title: nameTrimmed,
        namespace: modelscopeUser.username,
        dataset: repoId,
        endpoint: manualEndpoint
      });
      globalThis.localStorage?.setItem("lios.lastSpaceName", repoId);
      const scopedSpace = await refreshSetup(true);
      setCreateSpaceOpen(false);
      setNewSpaceName("");
      if (scopedSpace) await loadSpace(scopedSpace);
    } catch (error) {
      const text = errorText(error);
      setCreateSpaceError(text);
      await refreshSetup(false).catch(() => undefined);
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="appFrame">
      <header className="windowTitlebar">
        <div className="windowTitle" data-tauri-drag-region onMouseDown={startWindowDrag}>
          <img src={liosPetalMark} alt="" />
          <span>Lios</span>
        </div>
        <div className="windowDragRegion" data-tauri-drag-region onMouseDown={startWindowDrag} />
        <div className="windowControls">
          <button onClick={minimizeWindow} title="最小化" aria-label="最小化">
            <Minus aria-hidden />
          </button>
          <button onClick={toggleMaximizeWindow} title="最大化" aria-label="最大化">
            <Square aria-hidden />
          </button>
          <button className="closeWindow" onClick={closeWindow} title="关闭" aria-label="关闭">
            <X aria-hidden />
          </button>
        </div>
      </header>

      <main className="driveShell">
        <aside className="spaceRail">
          <div className="accountSection">
            <span className="sectionLabel">账号</span>
            <button
              className={`accountItem ${hasToken ? "active" : "empty"}`}
              onClick={selectAccount}
              title="点击查看所有空间"
            >
              <KeyRound aria-hidden />
              <span>
                <strong>{modelscopeUser?.username ?? "未连接账号"}</strong>
                <small>{hasToken ? "ModelScope" : "未连接"}</small>
              </span>
            </button>
          </div>

          <div className="accountListSpacer" />

          <div className="railFooter">
            <button
              className={view === "spaces" || view === "drive" ? "active" : ""}
              onClick={() => {
                setView("spaces");
                setQuery("");
              }}
              title="空间"
            >
              <HardDrive aria-hidden />
              空间
            </button>
            <button
              className={view === "settings" ? "active" : ""}
              onClick={() => setView("settings")}
              title="设置"
            >
              <Settings aria-hidden />
              设置
            </button>
          </div>
        </aside>

        <section className="driveWorkspace">
          {view !== "settings" && (
            <header className="driveTopbar">
              <nav className="crumbs" aria-label="当前路径" title={fullBreadcrumbPath}>
                {view === "drive" && (
                  <>
                    <button
                      type="button"
                      className="crumbRootBtn"
                      onClick={() => {
                        setView("spaces");
                        setQuery("");
                      }}
                      title="空间列表"
                      aria-label="返回空间列表"
                    >
                      <HardDrive aria-hidden />
                      <span>空间列表</span>
                    </button>
                    {crumbs.length > 0 && <ChevronRight aria-hidden className="crumbSeparator" />}
                  </>
                )}
                {crumbs.length > 0 ? (
                  crumbs.map((crumb, index) => (
                    <button
                      key={crumb.id}
                      type="button"
                      onClick={() => setCurrentFolderId(crumb.id)}
                      className={index === crumbs.length - 1 ? "current" : ""}
                      title={crumbPaths[index]}
                      aria-label={`${index === crumbs.length - 1 ? "当前路径" : "转到路径"}：${crumbPaths[index]}`}
                      aria-current={index === crumbs.length - 1 ? "page" : undefined}
                    >
                      {index > 0 && <ChevronRight aria-hidden />}
                      <span className="crumbLabel">{crumb.name}</span>
                    </button>
                  ))
                ) : (
                  <span className="crumbFallback" title={crumbFallbackLabel}>
                    {crumbFallbackLabel}
                  </span>
                )}
              </nav>
              <div className="searchBox">
                <Search aria-hidden />
                <input
                  value={query}
                  onChange={(event) => {
                    setQuery(event.target.value);
                    if (view === "drive" && !event.target.value.trim()) setSearchResults([]);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" && view === "drive") searchCatalog();
                  }}
                  placeholder={view === "spaces" ? "搜索空间" : "搜索当前空间"}
                />
              </div>
            </header>
          )}

          {message && (
            <div className="noticeBar">
              <AlertTriangle aria-hidden />
              <span>{message}</span>
              <button onClick={() => setMessage("")} title="关闭">
                <X aria-hidden />
              </button>
            </div>
          )}

          {view === "settings" ? (
            <section className="settingsPage">
              <div className="settingsBlock">
                <div>
                  <h2>连接</h2>
                  <p>
                    {modelscopeUser?.username
                      ? `已连接 ${modelscopeUser.username}`
                      : "输入访问凭证连接 ModelScope 账号"}
                  </p>
                </div>
                <div className="connectionGrid">
                  <input
                    type="password"
                    value={token}
                    onChange={(event) => setToken(event.target.value)}
                    placeholder="ModelScope access token"
                    autoComplete="off"
                  />
                  <input
                    className="endpointInput"
                    value={manualEndpoint}
                    onChange={(event) => setManualEndpoint(event.target.value)}
                    placeholder="服务地址"
                  />
                  <button
                    className="primary"
                    onClick={saveToken}
                    disabled={!token || !manualEndpoint || busy !== null}
                  >
                    <ShieldCheck aria-hidden />
                    连接
                  </button>
                </div>
              </div>

              <div className="settingsBlock recoveryKeyBlock">
                <div className="settingsHeaderRow">
                  <div>
                    <h2>恢复密钥</h2>
                    <p>用于打开已加密的 Lios 空间</p>
                  </div>
                  <div className="settingsActions">
                    <button
                      onClick={exportRecoveryKey}
                      disabled={!snapshot?.recovery_key.key_location || busy !== null}
                    >
                      <Download aria-hidden />
                      导出备份
                    </button>
                    <button
                      onClick={selectRecoveryKeyForImport}
                      disabled={busy !== null || recoveryKeyImport !== null}
                    >
                      <UploadCloud aria-hidden />
                      导入密钥
                    </button>
                  </div>
                </div>
                <div className="recoveryKeySummary">
                  <div className="recoveryKeyLocation">
                    <KeyRound aria-hidden />
                    <span>
                      <small>当前密钥</small>
                      <strong title={snapshot?.recovery_key.key_location ?? undefined}>
                        {conciseRecoveryKeyPath(snapshot?.recovery_key.key_location)}
                      </strong>
                    </span>
                  </div>
                  <span
                    className={`backupStatus ${snapshot?.recovery_key.backed_up ? "ready" : "missing"}`}
                    title={snapshot?.recovery_key.backup_location ?? undefined}
                  >
                    {recoveryKeyBackupText(snapshot?.recovery_key)}
                  </span>
                </div>
              </div>

              <div className="settingsBlock">
                <div className="settingsHeaderRow">
                  <div>
                    <h2>空间检查</h2>
                    <p>
                      {selectedSpace
                        ? `${selectedSpace.namespace}/${selectedSpace.dataset}`
                        : "未选择空间"}
                    </p>
                  </div>
                  <div className="settingsActions">
                    <button
                      onClick={() => verifySpace(false)}
                      disabled={!selectedSpace || busy !== null}
                    >
                      <ShieldCheck aria-hidden />
                      快速检查
                    </button>
                    <button
                      onClick={() => verifySpace(true)}
                      disabled={!selectedSpace || busy !== null}
                    >
                      <HardDrive aria-hidden />
                      完整检查
                    </button>
                    <button
                      onClick={openCatalogRebuildDialog}
                      disabled={
                        !selectedSpace || busy !== null || rebuildDialog !== null || rebuildTaskActive
                      }
                    >
                      <RefreshCw aria-hidden />
                      重建 Catalog
                    </button>
                  </div>
                </div>
              </div>

              <div className="settingsBlock">
                <div className="settingsHeaderRow">
                  <div>
                    <h2>传输分块</h2>
                    <p>设置大文件上传的分块大小（弱网环境下建议使用较小分块以提升断点重传成功率）</p>
                  </div>
                  <select
                    value={snapshot?.config.chunk_size ?? 134217728}
                    onChange={async (event) => {
                      const size = Number(event.target.value);
                      try {
                        await appInvoke("set_chunk_size", { chunkSizeBytes: size });
                        await refreshSetup(false);
                      } catch (error) {
                        setMessage(errorText(error));
                      }
                    }}
                    disabled={busy !== null || activeTasks > 0}
                  >
                    <option value={16777216}>16 MB (弱网快速重传)</option>
                    <option value={33554432}>32 MB (推荐 / 均衡)</option>
                    <option value={67108864}>64 MB (高速网络)</option>
                    <option value={134217728}>128 MB (超大单块)</option>
                  </select>
                </div>
              </div>

              <div className="settingsBlock">
                <div className="settingsHeaderRow">
                  <div>
                    <h2>本地状态</h2>
                  </div>
                  <button onClick={cleanupLocalCache} disabled={busy !== null || activeTasks > 0}>
                    <Trash2 aria-hidden />
                    清理缓存
                  </button>
                </div>
                {cacheCleanup && (
                  <div className="cleanupResult">
                    已清理 {cacheCleanup.files_removed} 个文件、{cacheCleanup.dirs_removed} 个空目录，释放{" "}
                    {formatCacheBytes(cacheCleanup.bytes_removed)}
                  </div>
                )}
                <dl className="pathGrid">
                  <div>
                    <dt>Config</dt>
                    <dd>{displayPath(snapshot?.paths.config)}</dd>
                  </div>
                  <div>
                    <dt>Database</dt>
                    <dd>{displayPath(snapshot?.paths.database)}</dd>
                  </div>
                  <div>
                    <dt>Staging</dt>
                    <dd>{displayPath(snapshot?.paths.staging)}</dd>
                  </div>
                  <div>
                    <dt>Logs</dt>
                    <dd title={snapshot?.paths.logs ?? undefined}>
                      {conciseRecoveryKeyPath(snapshot?.paths.logs)}
                    </dd>
                  </div>
                </dl>
              </div>

              <AboutSection loadVersion={loadAppVersion} />
            </section>
          ) : view === "spaces" ? (
            <SpaceGrid
              accountName={accountName}
              hasToken={hasToken}
              spaces={visibleSpaces}
              activeSpace={activeSpace}
              query={query}
              busy={busy !== null}
              onRefresh={() => refreshSetup(true)}
              onCreateSpace={openCreateSpaceDialog}
              onSelectSpace={(space) => loadSpace(space)}
              onRemoveSpace={handleRemoveSpace}
              onOpenSettings={() => setView("settings")}
            />
          ) : (
            <>
              <DriveToolbar
                canUpload={Boolean(catalogTree)}
                selectedCount={selectedCount}
                totalCount={visibleItems.length}
                viewMode={viewMode}
                busy={busy !== null}
                onUploadFiles={() => pickUpload(false)}
                onUploadFolder={() => pickUpload(true)}
                onNewFolder={createFolder}
                onDownload={downloadSelected}
                onRename={renameSelected}
                onDelete={deleteSelected}
                onRefresh={() => reloadCatalog()}
                onClearSelection={() => {
                  setSelectedIds(new Set());
                  setLastSelectedId(null);
                }}
                onViewModeChange={handleViewModeChange}
              />

              <section
                className="fileSurface"
                onDragOver={(event) => {
                  event.preventDefault();
                  if (activeSpace && currentFolderId) setIsDragging(true);
                }}
                onDragLeave={(event) => {
                  if (event.currentTarget.contains(event.relatedTarget as Node)) return;
                  setIsDragging(false);
                }}
                onDrop={(event) => {
                  event.preventDefault();
                  setIsDragging(false);
                }}
              >
                <DragDropOverlay
                  isDragging={isDragging}
                  targetDirName={currentFolder?.name || activeSpace?.title || activeSpace?.dataset}
                />

                {!activeSpace ? (
                  <div className="emptyDrive">
                    {emptyDriveMode === "create" ? (
                      <HardDrive aria-hidden />
                    ) : (
                      <UploadCloud aria-hidden />
                    )}
                    <h2>
                      {emptyDriveMode === "create"
                        ? "创建一个空间"
                        : emptyDriveMode === "connect"
                          ? "连接 ModelScope"
                          : "选择一个空间"}
                    </h2>
                    {emptyDriveMode !== "select" && (
                      <button
                        className="primary"
                        onClick={
                          emptyDriveMode === "create"
                            ? openCreateSpaceDialog
                            : () => setView("settings")
                        }
                      >
                        {emptyDriveMode === "create" ? (
                          <UploadCloud aria-hidden />
                        ) : (
                          <Settings aria-hidden />
                        )}
                        <span>{emptyDriveMode === "create" ? "创建空间" : "设置令牌"}</span>
                      </button>
                    )}
                  </div>
                ) : catalogStatus === "loading" || catalogStatus === "idle" ? (
                  <div className="emptyDrive">
                    <RefreshCw className="loadingGlyph" aria-hidden />
                    <h2>正在打开空间</h2>
                  </div>
                ) : catalogStatus === "missing" ? (
                  <div className="emptyDrive">
                    <HardDrive aria-hidden />
                    <h2>{activeSpace.title || activeSpace.dataset}</h2>
                    <button
                      className="primary"
                      onClick={initializeActiveSpace}
                      disabled={busy !== null}
                    >
                      <ShieldCheck aria-hidden />
                      初始化空间
                    </button>
                  </div>
                ) : catalogStatus === "error" || !catalogTree ? (
                  <div className="emptyDrive">
                    <AlertTriangle aria-hidden />
                    <h2>打开空间失败</h2>
                    <button
                      className="primary"
                      onClick={() => reloadCatalog()}
                      disabled={busy !== null}
                    >
                      <RefreshCw aria-hidden />
                      重试
                    </button>
                  </div>
                ) : visibleItems.length === 0 ? (
                  <div className="emptyDrive">
                    <FolderOpen aria-hidden />
                    <h2>{query.trim() ? "没有搜索结果" : "此文件夹为空"}</h2>
                  </div>
                ) : viewMode === "grid" ? (
                  <FileGrid
                    items={sortedItems}
                    selectedIds={selectedIds}
                    onToggleSelect={toggleSelection}
                    onSelectOnly={selectOnly}
                    onSelectRange={selectRange}
                    onEnterItem={enterItem}
                    onContextMenu={handleContextMenu}
                  />
                ) : (
                  <FileTable
                    items={sortedItems}
                    selectedIds={selectedIds}
                    sortField={sortField}
                    sortDirection={sortDirection}
                    onSort={handleSort}
                    onToggleSelect={toggleSelection}
                    onSelectOnly={selectOnly}
                    onSelectRange={selectRange}
                    onSelectAll={selectAll}
                    onEnterItem={enterItem}
                    onContextMenu={handleContextMenu}
                  />
                )}
              </section>
            </>
          )}

          <TaskCenter
            tasks={tasks}
            pendingActions={pendingActions}
            onAction={runTaskAction}
            listTaskItems={listTaskItems}
            onError={handleTaskError}
          />
        </section>

        <ContextMenu
          state={contextMenu}
          onClose={closeContextMenu}
          selectedCount={selectedCount}
          onOpenItem={enterItem}
          onPreviewItem={openFilePreview}
          onDownload={downloadSelected}
          onRename={renameSelected}
          onDelete={deleteSelected}
          onUploadFiles={() => pickUpload(false)}
          onUploadFolder={() => pickUpload(true)}
          onNewFolder={createFolder}
          onRefresh={() => reloadCatalog()}
          onSelectAll={selectAll}
        />

        <RebuildCatalogModal
          dialog={rebuildDialog}
          rebuildTaskActive={rebuildTaskActive}
          onClose={closeCatalogRebuildDialog}
          onReloadPreview={() => rebuildDialog && loadCatalogRebuildPreview(rebuildDialog.space)}
          onConfirm={confirmCatalogRebuild}
        />

        <ImportKeyModal
          dialog={recoveryKeyImport}
          onClose={() => setRecoveryKeyImport(null)}
          onConfirm={confirmRecoveryKeyImport}
        />

        <ConflictModal
          conflicts={conflicts}
          conflictActions={conflictActions}
          onSetAction={handleConflictAction}
          onSetAllActions={handleSetAllConflictActions}
          onCancel={() => {
            setConflicts([]);
            setPendingUpload([]);
          }}
          onConfirm={confirmConflicts}
        />

        <CreateSpaceModal
          open={createSpaceOpen}
          name={newSpaceName}
          error={createSpaceError}
          busy={busy !== null}
          onChangeName={(val) => {
            setNewSpaceName(val);
            setCreateSpaceError("");
          }}
          onClose={() => {
            setCreateSpaceOpen(false);
            setCreateSpaceError("");
          }}
          onSubmit={submitCreateSpace}
        />

        <FilePreviewModal
          open={previewOpen}
          item={previewItem}
          loading={previewLoading}
          error={previewError}
          content={previewContent}
          hasPrev={hasPrevPreview}
          hasNext={hasNextPreview}
          onPrev={handlePrevPreview}
          onNext={handleNextPreview}
          onClose={() => {
            setPreviewOpen(false);
            setPreviewItem(null);
          }}
          onDownload={() => {
            if (previewItem) {
              setSelectedIds(new Set([previewItem.id]));
              void downloadSelected();
            }
          }}
        />
      </main>
    </div>
  );
}

export default App;
