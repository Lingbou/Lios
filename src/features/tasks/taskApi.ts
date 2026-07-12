import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { TaskAction, TaskApi, TaskItemsPage, TaskSummary, TaskUpdateEvent } from "./taskTypes.ts";

type InvokeTask = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
type SubscribeTasks = (listener: (tasks: TaskSummary[]) => void) => Promise<() => void>;

const actionCommands: Record<TaskAction, string> = {
  pause: "pause_task",
  resume: "resume_task",
  retry: "retry_task",
  cancel: "cancel_task",
  clear: "clear_task"
};

async function subscribeToTauriTasks(listener: (tasks: TaskSummary[]) => void) {
  return listen<TaskUpdateEvent>("lios-tasks-updated", (event) => listener(event.payload.tasks));
}

export function createTaskApi(
  invokeTask: InvokeTask = invoke,
  subscribeTasks: SubscribeTasks = subscribeToTauriTasks
): TaskApi {
  return {
    listTasks: () => invokeTask<TaskSummary[]>("list_tasks"),
    listTaskItems: (taskId, offset, limit) =>
      invokeTask<TaskItemsPage>("list_task_items", { taskId, offset, limit }),
    runAction: (action, taskId) =>
      invokeTask<TaskSummary[]>(actionCommands[action], { taskId }),
    subscribe: subscribeTasks
  };
}
