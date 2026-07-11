import assert from "node:assert/strict";
import test from "node:test";

import {
  taskItemProgressPercent,
  taskItemStatusText,
  taskProgressPercent,
  taskProgressText,
  taskStatusText
} from "../src/taskPresentation.ts";

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
      state: "Preparing",
      label: "verify_full",
      phase: null,
      progress_total: 0,
      progress_done: 0
    }),
    "正在准备空间检查"
  );
});
