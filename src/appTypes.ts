import type {
  RecoveryKeyStatus,
  RecoveryKeyVerification
} from "./recoveryKeyPresentation.ts";
import type { SetupWarning } from "./setupWarning.ts";

export type RepoConfig = {
  namespace: string;
  dataset: string;
  endpoint: string;
};

export type SpaceSummary = RepoConfig & {
  visibility?: string | null;
  updated_at?: string | null;
  description?: string | null;
  task_space_id?: string;
};

export type ModelScopeUserSummary = {
  username: string;
  email?: string | null;
};

export type DatasetRepoListResult = {
  user: ModelScopeUserSummary;
  repositories: SpaceSummary[];
};

export type LiosConfig = {
  active_repo?: RepoConfig | null;
  key_file_path?: string | null;
  backup_path?: string | null;
  chunk_size?: number | null;
};

export type PathsDto = {
  home: string;
  config: string;
  database: string;
  staging: string;
  logs: string;
  credentials: string;
};

export type CacheCleanupReport = {
  files_removed: number;
  dirs_removed: number;
  bytes_removed: number;
};

export type CatalogTreeNode = {
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

export type DriveItem = {
  id: string;
  name: string;
  kind: "Directory" | "File";
  size: number;
  updated_at: string;
  children_count: number;
};

export type CatalogLoadResult = {
  local_path: string;
  bytes: number;
  tree: CatalogTreeNode;
  warnings: string[];
};

export type CatalogRebuildReport = {
  nodes_rebuilt: number;
  directories_rebuilt: number;
  files_rebuilt: number;
  content_objects_rebuilt: number;
  chunks_referenced: number;
  original_bytes_referenced: number;
  unreferenced_managed_objects: number;
};

export type CatalogRebuildPreview = {
  revision: string;
  tree: CatalogTreeNode;
  report: CatalogRebuildReport;
  warnings: string[];
};

export type CatalogRebuildDialog = {
  space: RepoConfig;
  status: "loading" | "ready" | "submitting" | "error";
  preview: CatalogRebuildPreview | null;
  error: string;
};

export type RecoveryKeyImportDialog = {
  path: string;
  verification: RecoveryKeyVerification;
  importing: boolean;
  error: string;
};

export type Snapshot = {
  paths: PathsDto;
  config: LiosConfig;
  recovery_key: RecoveryKeyStatus;
  has_token: boolean;
  active_task_space_id: string | null;
  warning: SetupWarning | null;
};

export type UploadConflict = {
  source_path: string;
  target_name: string;
  existing_node_id: string;
  kind: "Directory" | "File";
};

export type ConflictAction = "Replace" | "KeepBoth" | "Skip";

export type ConflictResolution = {
  source_path: string;
  action: ConflictAction;
};
