export type TaskState =
  | "Queued"
  | "Preparing"
  | "Running"
  | "Paused"
  | "Retrying"
  | "Committing"
  | "Failed"
  | "Completed"
  | "Canceled";

export type TaskItemState =
  | "Queued"
  | "Running"
  | "Skipped"
  | "Failed"
  | "Completed"
  | "Canceled";

export type TaskAction = "pause" | "resume" | "retry" | "cancel" | "clear";

export type TaskSummary = {
  id: string;
  account_id: string;
  space_id: string;
  state: TaskState;
  label: string;
  phase?: string | null;
  progress_total: number;
  progress_done: number;
  bytes_total: number;
  bytes_done: number;
  speed_bps: number;
  eta_seconds?: number | null;
  attempt: number;
  created_at: string;
  updated_at: string;
  error?: string | null;
  item_count: number;
  can_retry: boolean;
};

export type TaskItem = {
  id: string;
  task_id: string;
  name: string;
  relative_path?: string | null;
  size: number;
  state: TaskItemState;
  phase?: string | null;
  bytes_done: number;
  bytes_total: number;
  error?: string | null;
};

export type TaskItemsPage = {
  task_id: string;
  offset: number;
  total: number;
  items: TaskItem[];
};

export type TaskUpdateEvent = {
  tasks: TaskSummary[];
};

export type TaskApi = {
  listTasks: () => Promise<TaskSummary[]>;
  listTaskItems: (taskId: string, offset: number, limit: number) => Promise<TaskItemsPage>;
  runAction: (action: TaskAction, taskId: string) => Promise<TaskSummary[]>;
  subscribe: (listener: (tasks: TaskSummary[]) => void) => Promise<() => void>;
};
