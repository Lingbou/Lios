import assert from "node:assert/strict";
import test from "node:test";

import {
  isActiveTask,
  isLiveTask,
  isTerminalTask,
  newCatalogMutationCompletions,
  seedCatalogMutationCompletions,
  taskItemProgressPercent,
  taskItemStatusText,
  taskLabelText,
  taskProgressPercent,
  taskProgressText,
  taskStatusText,
  taskActionsForTask
} from "../src/features/tasks/taskPresentation.ts";

test("task state predicates distinguish interactive, live, and terminal states", () => {
  assert.equal(isActiveTask({ state: "Paused" }), true);
  assert.equal(isLiveTask({ state: "Paused" }), false);
  assert.equal(isTerminalTask({ state: "Completed" }), true);

  for (const state of ["Queued", "Running", "Committing"] as const) {
    assert.equal(isActiveTask({ state }), true);
    assert.equal(isLiveTask({ state }), true);
    assert.equal(isTerminalTask({ state }), false);
  }

  for (const state of ["Failed", "Completed", "Canceled"] as const) {
    assert.equal(isActiveTask({ state }), false);
    assert.equal(isLiveTask({ state }), false);
    assert.equal(isTerminalTask({ state }), true);
  }
});

test("task states expose only their real supported actions", () => {
  assert.deepEqual(taskActionsForTask({ state: "Queued", can_retry: false }), ["pause", "cancel"]);
  assert.deepEqual(taskActionsForTask({ state: "Running", can_retry: false }), ["pause", "cancel"]);
  assert.deepEqual(taskActionsForTask({ state: "Paused", can_retry: false }), ["resume", "cancel"]);
  assert.deepEqual(taskActionsForTask({ state: "Failed", can_retry: false }), ["clear"]);
  assert.deepEqual(taskActionsForTask({ state: "Failed", can_retry: true }), ["retry", "clear"]);
  assert.deepEqual(taskActionsForTask({ state: "Completed", can_retry: false }), ["clear"]);
  assert.deepEqual(taskActionsForTask({ state: "Canceled", can_retry: false }), ["clear"]);
  assert.deepEqual(taskActionsForTask({ state: "Committing", can_retry: false }), []);
});

test("task progress prefers byte progress and includes speed plus ETA", () => {
  const task = {
    state: "Running",
    label: "upload",
    phase: "uploading",
    progress_done: 1,
    progress_total: 4,
    bytes_done: 1024 * 1024,
    bytes_total: 4 * 1024 * 1024,
    speed_bps: 512 * 1024,
    eta_seconds: 6
  } as const;

  assert.equal(taskProgressPercent(task), 25);
  assert.equal(taskProgressText(task), "1.00 MB / 4.00 MB · 25% · 512 KB/s · 剩余 6 秒");
});

test("task status describes local packing and remote transaction phases", () => {
  const running = {
    state: "Running",
    label: "upload",
    progress_total: 1,
    progress_done: 0
  } as const;

  assert.equal(taskStatusText({ ...running, phase: "preparing" }), "正在切片加密");
  assert.equal(taskStatusText({ ...running, phase: "validating" }), "正在校验远端对象");
  assert.equal(taskStatusText({ ...running, phase: "uploading" }), "正在同步到远端");
  assert.equal(taskStatusText({ ...running, phase: "prepublishing" }), "正在提交加密对象");
  assert.equal(
    taskStatusText({ ...running, phase: "checking_remote" }),
    "正在核对远端清单"
  );
  assert.equal(taskStatusText({ ...running, phase: "publishing" }), "正在发布目录索引");
  assert.equal(taskStatusText({ ...running, phase: "cleaning" }), "正在清理远端对象");
  assert.equal(taskStatusText({ ...running, phase: "downloading" }), "正在下载");
  assert.equal(taskStatusText({ ...running, phase: "restoring" }), "正在恢复到本地");
  assert.equal(taskStatusText({ ...running, phase: "deleting" }), "正在删除");
});

test("task item status describes packing and apply phases", () => {
  const item = {
    state: "Running",
    phase: null,
    bytes_done: 1,
    bytes_total: 2,
    size: 2,
    error: null
  } as const;

  assert.equal(taskItemStatusText({ ...item, phase: "preparing" }), "正在切片加密");
  assert.equal(taskItemStatusText({ ...item, phase: "downloading" }), "正在下载");
  assert.equal(taskItemStatusText({ ...item, phase: "restoring" }), "正在恢复");
  assert.equal(taskItemStatusText({ ...item, phase: "applying_plan" }), "正在应用变更");
  assert.equal(taskItemStatusText({ ...item, phase: "deleting" }), "正在删除");
  assert.equal(taskItemStatusText({ ...item, phase: "uploading" }), "正在上传");
});

test("task item presentation reports concrete file state and progress", () => {
  const item = {
    state: "Running",
    phase: "uploading",
    bytes_done: 64,
    bytes_total: 128,
    size: 128,
    error: null
  } as const;

  assert.equal(taskItemProgressPercent(item), 50);
  assert.equal(taskItemStatusText(item), "正在上传");
  assert.equal(taskItemStatusText({ ...item, phase: "prepared" }), "已加密，等待上传");
  assert.equal(taskItemStatusText({ ...item, phase: "committing" }), "正在提交");
  assert.equal(taskItemStatusText({ ...item, phase: "retrying" }), "等待重试");
  assert.equal(
    taskItemStatusText({ ...item, state: "Failed", error: "连接中断" }),
    "失败：连接中断"
  );
  assert.equal(taskItemStatusText({ ...item, state: "Canceled" }), "已取消");
});

test("terminal task progress does not show stale speed or ETA", () => {
  assert.equal(
    taskProgressText({
      state: "Completed",
      label: "download",
      progress_done: 1,
      progress_total: 1,
      bytes_done: 1024,
      bytes_total: 1024,
      speed_bps: 512,
      eta_seconds: 10
    }),
    "1.00 KB / 1.00 KB · 100%"
  );
});

test("completed task average speed uses the execution start, not queue time", () => {
  const bytes = 60 * 1024 * 1024;
  assert.equal(
    taskProgressText({
      state: "Completed",
      label: "upload",
      progress_done: 1,
      progress_total: 1,
      bytes_done: bytes,
      bytes_total: bytes,
      created_at: "2026-07-12T00:00:00Z",
      started_at: "2026-07-12T00:09:00Z",
      updated_at: "2026-07-12T00:10:00Z"
    }),
    "60 MB / 60 MB · 100% · 1.00 MB/s"
  );
});

test("completed task average speed falls back to creation time", () => {
  const bytes = 60 * 1024 * 1024;
  assert.equal(
    taskProgressText({
      state: "Completed",
      label: "upload",
      progress_done: 1,
      progress_total: 1,
      bytes_done: bytes,
      bytes_total: bytes,
      created_at: "2026-07-12T00:00:00Z",
      updated_at: "2026-07-12T00:00:10Z"
    }),
    "60 MB / 60 MB · 100% · 6.00 MB/s"
  );
});

test("completed task status preserves integrity warnings", () => {
  assert.equal(
    taskStatusText({
      state: "Completed",
      label: "verify_quick",
      phase: null,
      progress_total: 3,
      progress_done: 3,
      error: "1 个旧版分片缺少长度元数据",
      attempt: 0
    }),
    "已完成：1 个旧版分片缺少长度元数据"
  );
});

test("content verification does not report 100 percent before the final step", () => {
  assert.equal(
    taskProgressPercent({
      state: "Running",
      label: "verify_full",
      phase: "verifying_content",
      progress_total: 4,
      progress_done: 3,
      bytes_total: 1024,
      bytes_done: 1024
    }),
    75
  );
});

test("canceled task status preserves cleanup warnings", () => {
  assert.equal(
    taskStatusText({
      state: "Canceled",
      label: "verify_full",
      phase: null,
      progress_total: 4,
      progress_done: 2,
      error: "verification staging cleanup failed: access denied"
    }),
    "已取消：verification staging cleanup failed: access denied"
  );
});

test("verification preparing status is not described as file packing", () => {
  assert.equal(
    taskStatusText({
      state: "Running",
      label: "verify_full",
      phase: "preparing",
      progress_total: 0,
      progress_done: 0
    }),
    "正在准备空间检查"
  );
});

test("catalog rebuild statuses describe recovery work", () => {
  const task = {
    state: "Running",
    label: "rebuild",
    phase: "preparing",
    progress_total: 0,
    progress_done: 0
  } as const;

  assert.equal(taskStatusText(task), "正在准备目录重建");
  assert.equal(
    taskStatusText({ ...task, state: "Running", phase: "downloading_recovery_metadata" }),
    "正在下载恢复元数据"
  );
  assert.equal(
    taskStatusText({ ...task, state: "Running", phase: "rebuilding_catalog" }),
    "正在重建目录索引"
  );
});

test("catalog rebuild uses a recovery label", () => {
  assert.equal(taskLabelText("rebuild"), "目录恢复");
});

test("catalog mutation completions seed a baseline and report newly completed tasks once", () => {
  const handled = new Set<string>();
  const task = (
    id: string,
    state: "Running" | "Completed",
    label: string,
    space_id = "space-a"
  ) => ({
    id,
    state,
    label,
    space_id
  });

  seedCatalogMutationCompletions(handled, [
    task("old-upload", "Completed", "upload"),
    task("pending-delete", "Running", "delete 2 items"),
    task("old-download", "Completed", "download")
  ]);

  assert.deepEqual([...handled], ["old-upload"]);
  assert.deepEqual(
    newCatalogMutationCompletions(handled, [
      task("old-upload", "Completed", "upload"),
      task("pending-delete", "Completed", "delete 2 items"),
      task("fast-upload", "Completed", "upload"),
      task("recovery", "Completed", "rebuild"),
      task("download", "Completed", "download")
    ], "space-a"),
    ["pending-delete", "fast-upload", "recovery"]
  );
  assert.deepEqual(
    newCatalogMutationCompletions(handled, [
      task("pending-delete", "Completed", "delete 2 items"),
      task("fast-upload", "Completed", "upload"),
      task("recovery", "Completed", "rebuild")
    ], "space-a"),
    []
  );
  assert.deepEqual(newCatalogMutationCompletions(handled, [], "space-a"), []);
  assert.deepEqual(
    newCatalogMutationCompletions(
      handled,
      [task("fast-upload", "Completed", "upload")],
      "space-a"
    ),
    []
  );
});

test("catalog mutation completions reload only the active task space", () => {
  const handled = new Set<string>();
  const completed = (id: string, space_id: string) => ({
    id,
    space_id,
    state: "Completed" as const,
    label: "upload"
  });

  seedCatalogMutationCompletions(handled, []);
  assert.deepEqual(
    newCatalogMutationCompletions(
      handled,
      [completed("space-a-task", "space-a"), completed("space-b-task", "space-b")],
      "space-b"
    ),
    ["space-b-task"]
  );
  assert.deepEqual(
    newCatalogMutationCompletions(handled, [completed("space-a-task", "space-a")], "space-a"),
    []
  );
});
