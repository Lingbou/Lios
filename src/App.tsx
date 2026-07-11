import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  AlertTriangle,
  CheckCircle2,
  ChevronRight,
  Cloud,
  Download,
  Edit3,
  File,
  Folder,
  FolderOpen,
  HardDrive,
  KeyRound,
  Minus,
  Plus,
  RefreshCw,
  Search,
  Settings,
  ShieldCheck,
  Square,
  Trash2,
  UploadCloud,
  X,
  XCircle
} from "lucide-react";
import { type MouseEvent, useEffect, useRef, useState } from "react";
import liosPetalMark from "./assets/lios-petal-mark.svg";
import { initializeWithExistingCatalog, loadCatalogState } from "./catalogState.ts";
import { errorText } from "./commandError.ts";
import { setupWarningMessage, type SetupWarning } from "./setupWarning.ts";
import {
  taskItemProgressPercent,
  taskItemStatusText,
  taskProgressPercent,
  taskProgressText
} from "./taskPresentation.ts";

type RepoConfig = {
  namespace: string;
  dataset: string;
  endpoint: string;
};

type SpaceSummary = RepoConfig & {
  visibility?: string | null;
  updated_at?: string | null;
  description?: string | null;
};

type ModelScopeUserSummary = {
  username: string;
  email?: string | null;
};

type DatasetRepoListResult = {
  user: ModelScopeUserSummary;
  repositories: SpaceSummary[];
};

type LiosConfig = {
  active_repo?: RepoConfig | null;
  key_file_path?: string | null;
  chunk_size?: number | null;
};

type PathsDto = {
  home: string;
  config: string;
  database: string;
  staging: string;
  credentials: string;
};

type TaskState =
  | "Queued"
  | "Preparing"
  | "Running"
  | "Paused"
  | "Retrying"
  | "Committing"
  | "Failed"
  | "Completed"
  | "Canceled";

type TaskItem = {
  id: string;
  task_id: string;
  name: string;
  relative_path?: string | null;
  source_path?: string | null;
  source_modified_at_ns?: number | null;
  size: number;
  state: "Queued" | "Running" | "Skipped" | "Failed" | "Completed" | "Canceled";
  phase?: string | null;
  bytes_done: number;
  bytes_total: number;
  error?: string | null;
};

type TaskRecord = {
  id: string;
  account_id: string;
  space_id: string;
  state: TaskState;
  label: string;
  phase?: string | null;
  progress_total: number;
  progress_done: number;
  bytes_total?: number;
  bytes_done?: number;
  speed_bps?: number;
  eta_seconds?: number | null;
  attempt: number;
  created_at: string;
  updated_at: string;
  error?: string | null;
  items: TaskItem[];
};

type TaskUpdateEvent = {
  tasks: TaskRecord[];
};

type CacheCleanupReport = {
  files_removed: number;
  dirs_removed: number;
  bytes_removed: number;
};

type CatalogTreeNode = {
  id: string;
  name: string;
  updated_at: string;
  kind:
    | { type: "Directory"; children: CatalogTreeNode[] }
    | {
        type: "File";
        original_size: number;
        sha256: string;
        object_id: string;
        chunk_count: number;
      };
};

type DriveItem = {
  id: string;
  name: string;
  kind: "Directory" | "File";
  size: number;
  updated_at: string;
  children_count: number;
};

type CatalogLoadResult = {
  local_path: string;
  bytes: number;
  tree: CatalogTreeNode;
  warnings: string[];
};

type Snapshot = {
  paths: PathsDto;
  config: LiosConfig;
  has_token: boolean;
  tasks: TaskRecord[];
  warning: SetupWarning | null;
};

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

function previewSnapshot(): Snapshot {
  return {
    paths: {
      home: "~\\.lios",
      config: "~\\.lios\\config.yaml",
      database: "~\\.lios\\lios.db",
      staging: "~\\.lios\\staging",
      credentials: "~\\.lios\\credentials.enc"
    },
    config: {
      active_repo: null,
      key_file_path: "~\\.lios\\keys\\default.key",
      chunk_size: 134217728
    },
    has_token: false,
    tasks: [],
    warning: null
  };
}

async function appInvoke<T>(command: string, args?: InvokeArgs): Promise<T> {
  if (hasTauriRuntime()) return invoke<T>(command, args);
  if (command === "current_setup") return previewSnapshot() as T;
  if (command === "list_dataset_repos") {
    return { user: { username: "Preview" }, repositories: [] } as T;
  }
  throw new Error("这个操作需要在 Lios 桌面端中执行");
}

type UploadConflict = {
  source_path: string;
  target_name: string;
  existing_node_id: string;
  kind: "Directory" | "File";
};

type ConflictAction = "Replace" | "KeepBoth" | "Skip";

type ConflictResolution = {
  source_path: string;
  action: ConflictAction;
};

type View = "spaces" | "drive" | "settings";
type CatalogStatus = "idle" | "loading" | "ready" | "missing" | "error";

function formatBytes(bytes: number) {
  if (!bytes) return "0 B";
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = units.shift() ?? "KB";
  while (value >= 1024 && units.length > 0) {
    value /= 1024;
    unit = units.shift() ?? unit;
  }
  return `${value.toFixed(value >= 10 ? 1 : 2)} ${unit}`;
}

function formatDate(value?: string | null) {
  if (!value) return "-";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "-";
  return date.toLocaleString();
}

function displayPath(value?: string | null) {
  if (!value) return "-";
  return value.replace(/^\\\\\?\\/, "");
}

function formatCacheBytes(bytes: number) {
  return bytes <= 0 ? "0 B" : formatBytes(bytes);
}

function taskIcon(state: TaskState) {
  if (state === "Completed") return <CheckCircle2 aria-hidden />;
  if (state === "Failed" || state === "Canceled") return <XCircle aria-hidden />;
  return <RefreshCw aria-hidden />;
}

function taskLabel(label: string) {
  if (label === "upload") return "上传";
  if (label === "download") return "下载";
  if (label === "delete" || label.startsWith("delete ")) return "删除";
  if (label === "restore") return "恢复";
  return label;
}

function taskStatusText(task: TaskRecord) {
  if (task.state === "Queued") return "等待中";
  if (task.state === "Paused") return "已暂停：等待继续或取消";
  if (task.state === "Completed") return "已完成";
  if (task.state === "Canceled") return "已取消";
  if (task.state === "Failed") return task.error ? `失败：${task.error}` : "失败";
  if (task.state === "Preparing" || task.phase === "preparing") return "正在切片加密";
  if (task.state === "Running" && task.phase === "uploading") return "正在同步到远端";
  if (task.state === "Running" && task.phase === "downloading") return "正在下载";
  if (task.state === "Running" && task.phase === "restoring") return "正在恢复到本地";
  if (task.state === "Running") return task.progress_total > 0 ? "正在处理" : "正在准备";
  if (task.state === "Retrying") return `正在重试${task.attempt > 0 ? `（第 ${task.attempt} 次）` : ""}`;
  if (task.state === "Committing" && task.phase === "reconciling") return "正在核对远端提交结果";
  if (task.state === "Committing") return "正在提交远端变更";
  return "处理中";
}

function nodeKind(node: CatalogTreeNode): "Directory" | "File" {
  return node.kind.type === "Directory" ? "Directory" : "File";
}

function nodeSize(node: CatalogTreeNode) {
  return node.kind.type === "File" ? node.kind.original_size : 0;
}

function nodeChildrenCount(node: CatalogTreeNode) {
  return node.kind.type === "Directory" ? node.kind.children.length : 0;
}

function findNode(node: CatalogTreeNode | null, id: string | null): CatalogTreeNode | null {
  if (!node || !id) return node;
  if (node.id === id) return node;
  if (node.kind.type === "Directory") {
    for (const child of node.kind.children) {
      const found = findNode(child, id);
      if (found) return found;
    }
  }
  return null;
}

function findParentId(node: CatalogTreeNode | null, targetId: string): string | null {
  if (!node || node.kind.type !== "Directory") return null;
  for (const child of node.kind.children) {
    if (child.id === targetId) return node.id;
    const nested = findParentId(child, targetId);
    if (nested) return nested;
  }
  return null;
}

function breadcrumb(node: CatalogTreeNode | null, targetId: string | null): CatalogTreeNode[] {
  if (!node) return [];
  if (!targetId || node.id === targetId) return [node];
  if (node.kind.type === "Directory") {
    for (const child of node.kind.children) {
      const nested = breadcrumb(child, targetId);
      if (nested.length > 0) return [node, ...nested];
    }
  }
  return [];
}

function treeToDriveItem(node: CatalogTreeNode): DriveItem {
  return {
    id: node.id,
    name: node.name,
    kind: nodeKind(node),
    size: nodeSize(node),
    updated_at: node.updated_at,
    children_count: nodeChildrenCount(node)
  };
}

function sameSpace(left: RepoConfig | null | undefined, right: RepoConfig | null | undefined) {
  return Boolean(
    left &&
      right &&
      left.namespace === right.namespace &&
      left.dataset === right.dataset &&
      left.endpoint === right.endpoint
  );
}

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
  const [query, setQuery] = useState("");
  const [searchResults, setSearchResults] = useState<DriveItem[]>([]);
  const [token, setToken] = useState("");
  const [manualEndpoint, setManualEndpoint] = useState("https://modelscope.cn");
  const [createSpaceOpen, setCreateSpaceOpen] = useState(false);
  const [newSpaceName, setNewSpaceName] = useState("");
  const [createSpaceError, setCreateSpaceError] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const [pendingUpload, setPendingUpload] = useState<string[]>([]);
  const [conflicts, setConflicts] = useState<UploadConflict[]>([]);
  const [conflictActions, setConflictActions] = useState<Record<string, ConflictAction>>({});
  const [cacheCleanup, setCacheCleanup] = useState<CacheCleanupReport | null>(null);
  const previousTaskStates = useRef<Map<string, TaskState>>(new Map());

  const tasks = snapshot?.tasks ?? [];
  const configured = Boolean(snapshot?.has_token && snapshot.config.key_file_path);
  const currentFolder = findNode(catalogTree, currentFolderId);
  const children =
    currentFolder?.kind.type === "Directory" ? currentFolder.kind.children.map(treeToDriveItem) : [];
  const visibleItems = query.trim() ? searchResults : children;
  const crumbs = breadcrumb(catalogTree, currentFolderId);
  const selectedCount = selectedIds.size;
  const runningTasks = tasks.filter((task) =>
    ["Preparing", "Running", "Retrying", "Committing"].includes(task.state)
  ).length;
  const activeTasks = tasks.filter((task) =>
    ["Queued", "Preparing", "Running", "Paused", "Retrying", "Committing"].includes(task.state)
  ).length;
  const failedTasks = tasks.filter((task) => task.state === "Failed").length;
  const totalTasks = tasks.length;
  const hasToken = Boolean(snapshot?.has_token);

  const displayedSpaces = spaces;
  const visibleSpaces = query.trim()
    ? displayedSpaces.filter((space) =>
        `${space.dataset} ${space.namespace}`.toLowerCase().includes(query.trim().toLowerCase())
      )
    : displayedSpaces;
  const hasSpaces = displayedSpaces.length > 0;
  const emptyDriveMode = !hasToken ? "connect" : hasSpaces ? "select" : "create";
  const accountName = modelscopeUser?.username ?? "未连接账号";

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
    const configuredRepo = next.config.active_repo;
    let visibleActiveRepo = configuredRepo ?? null;
    setManualEndpoint((current) => configuredRepo?.endpoint || current);
    if (loadSpaces && next.has_token) {
      setSpacesLoaded(false);
      const result = await appInvoke<DatasetRepoListResult>("list_dataset_repos", {
        endpoint: configuredRepo?.endpoint || manualEndpoint
      });
      setModelscopeUser(result.user);
      setSpaces(result.repositories);
      setSpacesLoaded(true);
      if (configuredRepo && !result.repositories.some((space) => sameSpace(space, configuredRepo))) {
        visibleActiveRepo = null;
        setCatalogTree(null);
        setCatalogStatus("idle");
        setCurrentFolderId(null);
        setSelectedIds(new Set());
        setSearchResults([]);
        setQuery("");
        setMessage("");
      }
    } else if (
      spacesLoaded &&
      configuredRepo &&
      !spaces.some((space) => sameSpace(space, configuredRepo))
    ) {
      visibleActiveRepo = null;
    }

    if (visibleActiveRepo) {
      setActiveSpace(visibleActiveRepo);
    } else if (loadSpaces || spacesLoaded) {
      setActiveSpace(null);
      setCatalogTree(null);
      setCatalogStatus("idle");
      setCurrentFolderId(null);
    }
  }

  async function refreshTasks() {
    const nextTasks = await appInvoke<TaskRecord[]>("list_tasks");
    setSnapshot((current) => (current ? { ...current, tasks: nextTasks } : current));
  }

  async function run(label: string, action: () => Promise<unknown>) {
    setBusy(label);
    setMessage("");
    try {
      const pending = action();
      await refreshTasks().catch(() => undefined);
      await pending;
      await refreshSetup(false);
      await refreshTasks().catch(() => undefined);
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
    let completedMutation = false;
    const nextStates = new Map<string, TaskState>();
    for (const task of tasks) {
      const previous = previousTaskStates.current.get(task.id);
      if (
        previous !== undefined &&
        previous !== "Completed" &&
        task.state === "Completed" &&
        (task.label === "upload" || task.label === "delete")
      ) {
        completedMutation = true;
      }
      nextStates.set(task.id, task.state);
    }
    previousTaskStates.current = nextStates;
    if (completedMutation && activeSpace) {
      reloadCatalog().catch((error) => setMessage(errorText(error)));
    }
  }, [tasks, activeSpace?.namespace, activeSpace?.dataset, activeSpace?.endpoint]);

  useEffect(() => {
    let active = true;
    let removeListener: (() => void) | null = null;
    listen<TaskUpdateEvent>("lios-tasks-updated", (event) => {
      if (active) {
        setSnapshot((current) => (current ? { ...current, tasks: event.payload.tasks } : current));
      }
    })
      .then((unlisten) => {
        removeListener = unlisten;
      })
      .catch(() => undefined);
    const pull = async () => {
      try {
        const nextTasks = await appInvoke<TaskRecord[]>("list_tasks");
        if (active) {
          setSnapshot((current) => (current ? { ...current, tasks: nextTasks } : current));
        }
      } catch {
        // The main setup path surfaces command errors; task polling should not steal focus.
      }
    };
    const timer = window.setInterval(pull, 1000);
    pull();
    return () => {
      active = false;
      removeListener?.();
      window.clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    if (view === "drive" && activeSpace && catalogStatus === "idle") {
      void loadSpace(activeSpace);
    }
  }, [view, activeSpace, catalogStatus]);

  async function loadSpace(space: SpaceSummary) {
    setView("drive");
    setActiveSpace(space);
    setCatalogStatus("loading");
    setCatalogTree(null);
    setCurrentFolderId(null);
    setSelectedIds(new Set());
    setSearchResults([]);
    setQuery("");
    try {
      const outcome = await loadCatalogState(() =>
        appInvoke<CatalogLoadResult>("load_space_catalog", { space })
      );
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
      setCatalogTree(null);
      setCurrentFolderId(null);
      setCatalogStatus("error");
      setMessage(errorText(error));
    }
  }

  async function initializeActiveSpace() {
    if (!activeSpace) return;
    setBusy("初始化空间");
    setMessage("");
    try {
      await initializeWithExistingCatalog(
        async () => {
          const result = await appInvoke<CatalogLoadResult>("initialize_space", {
            space: activeSpace
          });
          setCatalogTree(result.tree);
          setCurrentFolderId(result.tree.id);
          setCatalogStatus("ready");
          setSelectedIds(new Set());
          setMessage(result.warnings.join("; "));
        },
        () => reloadCatalog(true)
      );
      await refreshSetup(false);
    } catch (error) {
      setMessage(errorText(error));
      await refreshTasks().catch(() => undefined);
    } finally {
      setBusy(null);
    }
  }

  async function reloadCatalog(rethrow = false) {
    if (!activeSpace) return;
    setCatalogStatus("loading");
    setMessage("");
    try {
      const outcome = await loadCatalogState(() =>
        appInvoke<CatalogLoadResult>("load_space_catalog", { space: activeSpace })
      );
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
      setMessage(result.warnings.join("; "));
      if (query.trim()) await searchCatalog(query);
    } catch (error) {
      setCatalogTree(null);
      setCurrentFolderId(null);
      setCatalogStatus("error");
      setMessage(errorText(error));
      if (rethrow) throw error;
    }
  }

  function toggleSelection(id: string) {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function enterItem(item: DriveItem) {
    if (item.kind === "Directory") {
      setCurrentFolderId(item.id);
      setSelectedIds(new Set());
      setQuery("");
      setSearchResults([]);
    } else {
      toggleSelection(item.id);
    }
  }

  async function pickUpload(directory: boolean) {
    const selected = await open({ directory, multiple: !directory });
    const paths = Array.isArray(selected)
      ? selected.filter((item): item is string => typeof item === "string")
      : typeof selected === "string"
        ? [selected]
        : [];
    if (paths.length === 0 || !currentFolderId) return;
    const found = await appInvoke<UploadConflict[]>("preview_upload_conflicts", {
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

  async function startUpload(paths: string[], resolutions: ConflictResolution[]) {
    if (!currentFolderId) return;
    await run("上传", async () => {
      await appInvoke("enqueue_upload_to_folder", {
        parentNodeId: currentFolderId,
        paths,
        conflictResolutions: resolutions
      });
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

  async function createFolder() {
    if (!currentFolderId) return;
    const name = window.prompt("文件夹名称");
    if (!name) return;
    await run("新建文件夹", async () => {
      const result = await appInvoke<CatalogLoadResult>("create_folder", {
        parentNodeId: currentFolderId,
        name
      });
      setCatalogTree(result.tree);
      setCatalogStatus("ready");
      setMessage(result.warnings.join("; "));
    });
  }

  async function renameSelected() {
    const [nodeId] = [...selectedIds];
    if (!nodeId) return;
    const node = findNode(catalogTree, nodeId);
    const newName = window.prompt("新名称", node?.name ?? "");
    if (!newName) return;
    await run("重命名", async () => {
      const result = await appInvoke<CatalogLoadResult>("rename_node", { nodeId, newName });
      setCatalogTree(result.tree);
      setCatalogStatus("ready");
      setSelectedIds(new Set());
      setMessage(result.warnings.join("; "));
    });
  }

  async function deleteSelected() {
    if (selectedIds.size === 0) return;
    const ok = window.confirm(`直接删除 ${selectedIds.size} 个项目？此操作不会进入回收站。`);
    if (!ok) return;
    const nodeIds = [...selectedIds];
    await run("删除", async () => {
      await appInvoke("enqueue_delete_nodes", { nodeIds });
    });
  }

  async function downloadSelected() {
    const nodeIds = [...selectedIds];
    if (nodeIds.length === 0) return;
    const output = await open({ directory: true, multiple: false });
    if (typeof output !== "string") return;
    await run("下载", async () => {
      await appInvoke("enqueue_download", { nodeIds, outputDir: output });
    });
  }

  async function searchCatalog(value = query) {
    const trimmed = value.trim();
    setQuery(value);
    if (!trimmed) {
      setSearchResults([]);
      return;
    }
    const results = await appInvoke<DriveItem[]>("search_catalog", { query: trimmed });
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
    const trimmed = newSpaceName.trim();
    if (!trimmed) {
      setCreateSpaceError("输入空间名称");
      return;
    }
    setBusy("创建空间");
    setMessage("");
    setCreateSpaceError("");
    try {
      const space: SpaceSummary = {
        namespace: modelscopeUser.username,
        dataset: trimmed,
        endpoint: manualEndpoint
      };
      await appInvoke("create_dataset_repo", space);
      await refreshSetup(true);
      setCreateSpaceOpen(false);
      setNewSpaceName("");
      await loadSpace(space);
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
            className={view !== "settings" ? "active" : ""}
            onClick={() => {
              if (!activeSpace) {
                setView("spaces");
              } else if (catalogStatus === "ready" || catalogStatus === "missing" || catalogStatus === "error") {
                setView("drive");
              } else {
                void loadSpace(activeSpace);
              }
            }}
          >
            <FolderOpen aria-hidden />
            文件
          </button>
          <button
            className={view === "settings" ? "active" : ""}
            onClick={() => setView("settings")}
          >
            <Settings aria-hidden />
            设置
          </button>
        </div>
      </aside>

      <section className="driveWorkspace">
        <header className="driveTopbar">
          <div className="crumbs">
            {crumbs.length > 0 ? (
              crumbs.map((crumb, index) => (
                <button
                  key={crumb.id}
                  onClick={() => setCurrentFolderId(crumb.id)}
                  className={index === crumbs.length - 1 ? "current" : ""}
                >
                  {index > 0 && <ChevronRight aria-hidden />}
                  {crumb.name}
                </button>
              ))
            ) : (
              <span>
                {view === "spaces"
                  ? accountName
                  : activeSpace
                  ? activeSpace.dataset
                  : emptyDriveMode === "create"
                    ? "创建一个空间"
                    : emptyDriveMode === "connect"
                      ? "连接 ModelScope"
                      : "选择一个空间"}
              </span>
            )}
          </div>
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
                <p>{modelscopeUser?.username ? `已连接 ${modelscopeUser.username}` : "输入访问凭证连接 ModelScope 账号"}</p>
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
                  <dt>Key</dt>
                  <dd>{displayPath(snapshot?.config.key_file_path)}</dd>
                </div>
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
              </dl>
            </div>
          </section>
        ) : view === "spaces" ? (
          <section className="accountSpacesPage">
            <div className="accountSpacesHeader">
              <div>
                <h2>{accountName}</h2>
                <span>{hasToken ? "ModelScope" : "未连接"}</span>
              </div>
              <div className="toolbar">
                <button onClick={() => refreshSetup(true)} disabled={!hasToken || busy !== null}>
                  <RefreshCw aria-hidden />
                  刷新
                </button>
                <button
                  className="primary"
                  onClick={openCreateSpaceDialog}
                  disabled={!hasToken || busy !== null}
                >
                  <Plus aria-hidden />
                  创建空间
                </button>
              </div>
            </div>

            <section className="spaceSurface">
              {!hasToken ? (
                <div className="emptyDrive">
                  <Cloud aria-hidden />
                  <h2>连接 ModelScope</h2>
                  <button className="primary" onClick={() => setView("settings")}>
                    <Settings aria-hidden />
                    设置令牌
                  </button>
                </div>
              ) : visibleSpaces.length === 0 ? (
                <div className="emptyDrive">
                  <HardDrive aria-hidden />
                  <h2>{query.trim() ? "没有匹配空间" : "创建一个空间"}</h2>
                  {!query.trim() && (
                    <button className="primary" onClick={openCreateSpaceDialog} disabled={busy !== null}>
                      <Plus aria-hidden />
                      创建空间
                    </button>
                  )}
                </div>
              ) : (
                <div className="spaceGrid">
                  {visibleSpaces.map((space) => {
                    const active =
                      activeSpace?.namespace === space.namespace &&
                      activeSpace?.dataset === space.dataset &&
                      activeSpace?.endpoint === space.endpoint;
                    return (
                      <button
                        className={`spaceCard ${active ? "active" : ""}`}
                        key={`${space.endpoint}/${space.namespace}/${space.dataset}`}
                        onClick={() => loadSpace(space)}
                      >
                        <HardDrive aria-hidden />
                        <span>
                          <strong>{space.dataset}</strong>
                          <small>{space.namespace}</small>
                        </span>
                        <ChevronRight aria-hidden />
                      </button>
                    );
                  })}
                </div>
              )}
            </section>
          </section>
        ) : (
          <>
            <section className="driveToolbar">
              <div className="toolbar">
                <button
                  className="primary"
                  onClick={() => pickUpload(false)}
                  disabled={!catalogTree || busy !== null}
                >
                  <UploadCloud aria-hidden />
                  上传文件
                </button>
                <button onClick={() => pickUpload(true)} disabled={!catalogTree || busy !== null}>
                  <FolderOpen aria-hidden />
                  上传文件夹
                </button>
                <button onClick={createFolder} disabled={!catalogTree || busy !== null}>
                  <Plus aria-hidden />
                  新建文件夹
                </button>
              </div>
              <div className="toolbar">
                <button onClick={downloadSelected} disabled={selectedCount === 0 || busy !== null}>
                  <Download aria-hidden />
                  下载
                </button>
                <button onClick={renameSelected} disabled={selectedCount !== 1 || busy !== null}>
                  <Edit3 aria-hidden />
                  重命名
                </button>
                <button
                  className="danger"
                  onClick={deleteSelected}
                  disabled={selectedCount === 0 || busy !== null}
                >
                  <Trash2 aria-hidden />
                  删除
                </button>
                <button onClick={() => reloadCatalog()} disabled={!activeSpace || busy !== null}>
                  <RefreshCw aria-hidden />
                  刷新
                </button>
              </div>
            </section>

            <section className="fileSurface">
              {!activeSpace ? (
                <div className="emptyDrive">
                  {emptyDriveMode === "create" ? <HardDrive aria-hidden /> : <Cloud aria-hidden />}
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
                      onClick={emptyDriveMode === "create" ? openCreateSpaceDialog : () => setView("settings")}
                    >
                      {emptyDriveMode === "create" ? <Plus aria-hidden /> : <Settings aria-hidden />}
                      {emptyDriveMode === "create" ? "创建空间" : "设置令牌"}
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
                  <h2>{activeSpace.dataset}</h2>
                  <button className="primary" onClick={initializeActiveSpace} disabled={busy !== null}>
                    <ShieldCheck aria-hidden />
                    初始化空间
                  </button>
                </div>
              ) : catalogStatus === "error" || !catalogTree ? (
                <div className="emptyDrive">
                  <AlertTriangle aria-hidden />
                  <h2>打开空间失败</h2>
                  <button className="primary" onClick={() => reloadCatalog()} disabled={busy !== null}>
                    <RefreshCw aria-hidden />
                    重试
                  </button>
                </div>
              ) : visibleItems.length === 0 ? (
                <div className="emptyDrive">
                  <Folder aria-hidden />
                  <h2>{query.trim() ? "没有搜索结果" : "此文件夹为空"}</h2>
                </div>
              ) : (
                <table className="fileTable">
                  <thead>
                    <tr>
                      <th aria-label="选择" />
                      <th>名称</th>
                      <th>类型</th>
                      <th>大小</th>
                      <th>修改时间</th>
                    </tr>
                  </thead>
                  <tbody>
                    {visibleItems.map((item) => (
                      <tr
                        key={item.id}
                        className={selectedIds.has(item.id) ? "selected" : ""}
                        onDoubleClick={() => enterItem(item)}
                      >
                        <td>
                          <input
                            type="checkbox"
                            checked={selectedIds.has(item.id)}
                            onChange={() => toggleSelection(item.id)}
                          />
                        </td>
                        <td>
                          <button className="fileName" onClick={() => enterItem(item)}>
                            {item.kind === "Directory" ? <Folder aria-hidden /> : <File aria-hidden />}
                            <span>{item.name}</span>
                          </button>
                        </td>
                        <td>{item.kind === "Directory" ? `${item.children_count} 项` : "文件"}</td>
                        <td>{item.kind === "File" ? formatBytes(item.size) : "-"}</td>
                        <td>{formatDate(item.updated_at)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </section>
          </>
        )}

        <section className="taskDrawer" aria-label="任务">
          <div className="taskHeader">
            <div>
              <span>任务</span>
              <small>传输中心</small>
            </div>
            <small>{runningTasks} 运行 / {failedTasks} 失败 / {totalTasks} 记录</small>
          </div>
          <div className="taskRows">
            {tasks.length === 0 ? (
              <div className="taskEmpty">暂无任务</div>
            ) : (
              tasks.map((task) => {
                const progress = taskProgressPercent(task);
                const isCancelable = [
                  "Queued",
                  "Preparing",
                  "Running",
                  "Paused",
                  "Retrying",
                ].includes(task.state);
                return (
                  <article className={`taskRow ${task.state.toLowerCase()}`} key={task.id}>
                    <div className="taskSummary">
                      <div className="taskIcon">{taskIcon(task.state)}</div>
                      <div className="taskInfo">
                        <strong>
                          {taskLabel(task.label)}
                          {task.items.length > 0 && <small>{task.items.length} 个文件</small>}
                        </strong>
                        <span>{taskStatusText(task)}</span>
                      </div>
                      <div className="taskProgress">
                        <div className="meter" aria-label={`${progress}%`}>
                          <span style={{ width: `${progress}%` }} />
                        </div>
                        <small>{taskProgressText(task)}</small>
                      </div>
                      <div className="taskButtons">
                        {task.state === "Failed" ? (
                          <>
                            <button
                              title="重试任务"
                              aria-label="重试任务"
                              onClick={() =>
                                run("重试任务", () => appInvoke("retry_task", { taskId: task.id }))
                              }
                            >
                              <RefreshCw aria-hidden />
                            </button>
                            <button
                              title="清除记录"
                              aria-label="清除记录"
                              onClick={() =>
                                run("清除任务记录", () => appInvoke("clear_task", { taskId: task.id }))
                              }
                            >
                              <Trash2 aria-hidden />
                            </button>
                          </>
                        ) : isCancelable ? (
                          <button
                            className="iconDanger"
                            title="取消任务"
                            aria-label="取消任务"
                            onClick={() => run("取消任务", () => appInvoke("cancel_task", { taskId: task.id }))}
                          >
                            <XCircle aria-hidden />
                          </button>
                        ) : (
                          <button
                            title="清除记录"
                            aria-label="清除记录"
                            onClick={() => run("清除任务记录", () => appInvoke("clear_task", { taskId: task.id }))}
                          >
                            <Trash2 aria-hidden />
                          </button>
                        )}
                      </div>
                    </div>
                    {task.items.length > 0 && (
                      <div className="taskFileList" aria-label={`${taskLabel(task.label)}文件明细`}>
                        {task.items.map((item) => {
                          const itemProgress = taskItemProgressPercent(item);
                          const itemTotal = item.bytes_total || item.size;
                          return (
                            <div className={`taskFileRow ${item.state.toLowerCase()}`} key={item.id}>
                              <File aria-hidden />
                              <div className="taskFileInfo">
                                <strong title={item.relative_path || item.name}>
                                  {(item.relative_path || item.name).replace(/\\/g, "/")}
                                </strong>
                                <span>{taskItemStatusText(item)}</span>
                              </div>
                              <div className="taskFileProgress">
                                <div className="meter" aria-label={`${itemProgress}%`}>
                                  <span style={{ width: `${itemProgress}%` }} />
                                </div>
                                <small>
                                  {formatBytes(item.bytes_done)} / {formatBytes(itemTotal)} · {itemProgress}%
                                </small>
                              </div>
                            </div>
                          );
                        })}
                      </div>
                    )}
                  </article>
                );
              })
            )}
          </div>
        </section>
      </section>

      {conflicts.length > 0 && (
        <div className="modalBackdrop">
          <section className="conflictModal">
            <div className="modalHeader">
              <div>
                <h2>处理同名项目</h2>
              </div>
              <button
                onClick={() => {
                  setConflicts([]);
                  setPendingUpload([]);
                }}
              >
                <X aria-hidden />
              </button>
            </div>
            <div className="conflictList">
              {conflicts.map((conflict) => (
                <label className="conflictItem" key={conflict.source_path}>
                  <span>
                    <strong>{conflict.target_name}</strong>
                    <small>{conflict.source_path}</small>
                  </span>
                  <select
                    value={conflictActions[conflict.source_path] || "KeepBoth"}
                    onChange={(event) =>
                      setConflictActions((current) => ({
                        ...current,
                        [conflict.source_path]: event.target.value as ConflictAction
                      }))
                    }
                  >
                    <option value="KeepBoth">保留两者</option>
                    <option value="Replace">替换</option>
                    <option value="Skip">跳过</option>
                  </select>
                </label>
              ))}
            </div>
            <div className="modalActions">
              <button
                onClick={() => {
                  setConflicts([]);
                  setPendingUpload([]);
                }}
              >
                取消
              </button>
              <button className="primary" onClick={confirmConflicts}>
                继续上传
              </button>
            </div>
          </section>
        </div>
      )}

      {createSpaceOpen && (
        <div className="modalBackdrop">
          <section className="spaceModal" role="dialog" aria-modal="true" aria-labelledby="create-space-title">
            <div className="modalHeader">
              <div>
                <h2 id="create-space-title">创建空间</h2>
              </div>
              <button
                onClick={() => {
                  setCreateSpaceOpen(false);
                  setCreateSpaceError("");
                }}
                title="关闭"
              >
                <X aria-hidden />
              </button>
            </div>
            <div className="spaceModalBody">
              <label>
                <span>空间名称</span>
                <input
                  autoFocus
                  value={newSpaceName}
                  onChange={(event) => {
                    setNewSpaceName(event.target.value);
                    setCreateSpaceError("");
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") submitCreateSpace();
                    if (event.key === "Escape") setCreateSpaceOpen(false);
                  }}
                  placeholder="例如 lios-backup"
                />
              </label>
              {createSpaceError && <div className="fieldError">{createSpaceError}</div>}
            </div>
            <div className="modalActions">
              <button
                onClick={() => {
                  setCreateSpaceOpen(false);
                  setCreateSpaceError("");
                }}
              >
                取消
              </button>
              <button
                className="primary"
                onClick={submitCreateSpace}
                disabled={!newSpaceName.trim() || busy !== null}
              >
                创建
              </button>
            </div>
          </section>
        </div>
      )}
      </main>
    </div>
  );
}

export default App;
