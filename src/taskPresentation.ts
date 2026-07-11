export type TaskPresentationState =
  | "Queued"
  | "Preparing"
  | "Running"
  | "Paused"
  | "Retrying"
  | "Committing"
  | "Failed"
  | "Completed"
  | "Canceled";

export type TaskPresentationRecord = {
  state: TaskPresentationState;
  label: string;
  phase?: string | null;
  progress_total: number;
  progress_done: number;
  bytes_total?: number;
  bytes_done?: number;
  speed_bps?: number;
  eta_seconds?: number | null;
};

export type TaskItemPresentationRecord = {
  state: "Queued" | "Running" | "Skipped" | "Failed" | "Completed" | "Canceled";
  phase?: string | null;
  bytes_done: number;
  bytes_total: number;
  size: number;
  error?: string | null;
};

function finiteNonNegative(value: number | null | undefined) {
  return Number.isFinite(value) && (value ?? 0) > 0 ? (value ?? 0) : 0;
}

function formatTransferBytes(bytes: number) {
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
    parts.push(`${formatTransferBytes(bytesDone)} / ${formatTransferBytes(bytesTotal)}`);
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
    if (speed > 0) parts.push(`${formatTransferBytes(speed)}/s`);
    const eta = finiteNonNegative(task.eta_seconds);
    if (eta > 0) parts.push(`剩余 ${formatEta(eta)}`);
  }
  return parts.join(" · ");
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
