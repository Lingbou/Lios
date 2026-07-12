import type { TaskAction, TaskItem, TaskSummary } from "./taskTypes.ts";

export type TaskPresentationRecord = Pick<
  TaskSummary,
  "state" | "label" | "progress_total" | "progress_done"
> &
  Partial<
    Pick<
      TaskSummary,
      "phase" | "bytes_total" | "bytes_done" | "speed_bps" | "eta_seconds" | "attempt" | "error"
    >
  >;

export type TaskItemPresentationRecord = Pick<
  TaskItem,
  "state" | "phase" | "bytes_done" | "bytes_total" | "size" | "error"
>;

type CatalogMutationTask = Pick<TaskSummary, "id" | "space_id" | "state" | "label">;

function isCompletedCatalogMutation(task: CatalogMutationTask) {
  return (
    task.state === "Completed" &&
    (task.label === "upload" ||
      task.label === "delete" ||
      task.label.startsWith("delete ") ||
      task.label === "rebuild")
  );
}

export function seedCatalogMutationCompletions(
  handled: Set<string>,
  tasks: CatalogMutationTask[]
) {
  for (const task of tasks) {
    if (isCompletedCatalogMutation(task)) handled.add(task.id);
  }
}

export function newCatalogMutationCompletions(
  handled: Set<string>,
  tasks: CatalogMutationTask[],
  activeSpaceId: string
) {
  const completed: string[] = [];
  for (const task of tasks) {
    if (!isCompletedCatalogMutation(task) || handled.has(task.id)) continue;
    handled.add(task.id);
    if (task.space_id === activeSpaceId) completed.push(task.id);
  }
  return completed;
}

export function taskActionsForTask(
  task: Pick<TaskSummary, "state" | "can_retry">
): TaskAction[] {
  const { state } = task;
  if (["Queued", "Preparing", "Running", "Retrying"].includes(state)) {
    return ["pause", "cancel"];
  }
  if (state === "Paused") return ["resume", "cancel"];
  if (state === "Failed") return task.can_retry ? ["retry", "clear"] : ["clear"];
  if (state === "Completed" || state === "Canceled") return ["clear"];
  return [];
}

export function taskLabelText(label: string) {
  if (label === "upload") return "上传";
  if (label === "download") return "下载";
  if (label === "delete" || label.startsWith("delete ")) return "删除";
  if (label === "restore") return "恢复";
  if (label === "verify_quick") return "快速检查";
  if (label === "verify_full") return "完整检查";
  if (label === "rebuild") return "目录恢复";
  return label;
}

function finiteNonNegative(value: number | null | undefined) {
  return Number.isFinite(value) && (value ?? 0) > 0 ? (value ?? 0) : 0;
}

export function formatTaskBytes(bytes: number) {
  const safeBytes = Math.max(0, bytes);
  if (safeBytes < 1024) return `${Math.round(safeBytes)} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = safeBytes / 1024;
  let unit = units[0];
  for (let index = 1; value >= 1024 && index < units.length; index += 1) {
    value /= 1024;
    unit = units[index];
  }
  return `${value.toFixed(value >= 10 ? 0 : 2)} ${unit}`;
}

function formatEta(seconds: number) {
  const safeSeconds = Math.max(0, Math.round(seconds));
  if (safeSeconds < 60) return `${safeSeconds} 秒`;
  const hours = Math.floor(safeSeconds / 3600);
  const minutes = Math.floor((safeSeconds % 3600) / 60);
  const remainingSeconds = safeSeconds % 60;
  if (hours > 0) return `${hours} 小时 ${minutes} 分`;
  return remainingSeconds > 0 ? `${minutes} 分 ${remainingSeconds} 秒` : `${minutes} 分`;
}

export function taskProgressPercent(task: TaskPresentationRecord) {
  if (task.phase === "verifying_content" && task.progress_total > 0) {
    return Math.min(
      100,
      Math.max(0, Math.round((task.progress_done / task.progress_total) * 100))
    );
  }
  const bytesTotal = finiteNonNegative(task.bytes_total);
  const bytesDone = Math.max(0, task.bytes_done ?? 0);
  if (bytesTotal > 0) {
    return Math.min(100, Math.max(0, Math.round((bytesDone / bytesTotal) * 100)));
  }
  if (task.progress_total <= 0) return task.state === "Completed" ? 100 : 0;
  return Math.min(
    100,
    Math.max(0, Math.round((task.progress_done / task.progress_total) * 100))
  );
}

export function taskProgressText(task: TaskPresentationRecord) {
  const percent = taskProgressPercent(task);
  const bytesTotal = finiteNonNegative(task.bytes_total);
  const bytesDone = Math.max(0, task.bytes_done ?? 0);
  const parts: string[] = [];
  if (bytesTotal > 0) {
    parts.push(`${formatTaskBytes(bytesDone)} / ${formatTaskBytes(bytesTotal)}`);
    parts.push(`${percent}%`);
  } else if (task.progress_total > 0) {
    parts.push(`${task.progress_done}/${task.progress_total} 项`);
    parts.push(`${percent}%`);
  } else if (["Preparing", "Running", "Retrying", "Committing"].includes(task.state)) {
    parts.push("准备中");
  } else if (task.state === "Queued") {
    parts.push("等待开始");
  } else {
    parts.push("-");
  }
  const showsLiveRate = ["Preparing", "Running", "Retrying", "Committing"].includes(task.state);
  if (showsLiveRate) {
    const speed = finiteNonNegative(task.speed_bps);
    if (speed > 0) parts.push(`${formatTaskBytes(speed)}/s`);
    const eta = finiteNonNegative(task.eta_seconds);
    if (eta > 0) parts.push(`剩余 ${formatEta(eta)}`);
  }
  return parts.join(" · ");
}

export function taskStatusText(task: TaskPresentationRecord) {
  if (task.state === "Queued") return "等待中";
  if (task.state === "Paused") return "已暂停：等待继续或取消";
  if (task.state === "Completed") return task.error ? `已完成：${task.error}` : "已完成";
  if (task.state === "Canceled") return task.error ? `已取消：${task.error}` : "已取消";
  if (task.state === "Failed") return task.error ? `失败：${task.error}` : "失败";
  if (task.state === "Preparing" && task.label.startsWith("verify_")) {
    return "正在准备空间检查";
  }
  if (task.state === "Preparing" && task.label === "rebuild") return "正在准备目录重建";
  if (task.state === "Preparing" || task.phase === "preparing") return "正在切片加密";
  if (task.state === "Running" && task.phase === "uploading") return "正在同步到远端";
  if (task.state === "Running" && task.phase === "downloading") return "正在下载";
  if (task.state === "Running" && task.phase === "checking_remote") return "正在核对远端清单";
  if (task.state === "Running" && task.phase === "downloading_verification_data") {
    return "正在下载校验数据";
  }
  if (task.state === "Running" && task.phase === "verifying_content") return "正在验证加密内容";
  if (task.state === "Running" && task.phase === "checking_complete") return "检查完成";
  if (task.state === "Running" && task.phase === "downloading_recovery_metadata") {
    return "正在下载恢复元数据";
  }
  if (task.state === "Running" && task.phase === "rebuilding_catalog") {
    return "正在重建目录索引";
  }
  if (task.state === "Running" && task.phase === "restoring") return "正在恢复到本地";
  if (task.state === "Running") return task.progress_total > 0 ? "正在处理" : "正在准备";
  if (task.state === "Retrying") {
    return `正在重试${(task.attempt ?? 0) > 0 ? `（第 ${task.attempt} 次）` : ""}`;
  }
  if (task.state === "Committing" && task.phase === "reconciling") return "正在核对远端提交结果";
  if (task.state === "Committing") return "正在提交远端变更";
  return "处理中";
}

export function taskItemProgressPercent(item: TaskItemPresentationRecord) {
  const total = finiteNonNegative(item.bytes_total) || finiteNonNegative(item.size);
  if (total <= 0) return item.state === "Completed" ? 100 : 0;
  return Math.min(100, Math.max(0, Math.round((Math.max(0, item.bytes_done) / total) * 100)));
}

export function taskItemStatusText(item: TaskItemPresentationRecord) {
  if (item.state === "Queued") return "等待处理";
  if (item.state === "Skipped") return item.error ? `已跳过：${item.error}` : "已跳过";
  if (item.state === "Failed") return item.error ? `失败：${item.error}` : "失败";
  if (item.state === "Canceled") return "已取消";
  if (item.state === "Completed") return "已完成";
  if (item.phase === "preparing") return "正在切片加密";
  if (item.phase === "prepared") return "已加密，等待上传";
  if (item.phase === "uploading") return "正在上传";
  if (item.phase === "committing") return "正在提交";
  if (item.phase === "retrying") return "等待重试";
  if (item.phase === "downloading") return "正在下载";
  if (item.phase === "restoring") return "正在恢复";
  return "正在处理";
}

export function taskItemProgressText(item: TaskItemPresentationRecord) {
  const total = finiteNonNegative(item.bytes_total) || finiteNonNegative(item.size);
  return `${formatTaskBytes(item.bytes_done)} / ${formatTaskBytes(total)} · ${taskItemProgressPercent(item)}%`;
}
