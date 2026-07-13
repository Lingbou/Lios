import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { TaskAction, TaskApi, TaskItemsPage, TaskSummary, TaskUpdateEvent } from "./taskTypes.ts";

type InvokeTask = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
type SubscribeTasks = (listener: (event: TaskUpdateEvent) => void) => Promise<() => void>;

const actionCommands: Record<TaskAction, string> = {
  pause: "pause_task",
  resume: "resume_task",
  retry: "retry_task",
  cancel: "cancel_task",
  clear: "clear_task"
};

async function subscribeToTauriTasks(listener: (event: TaskUpdateEvent) => void) {
  return listen<TaskUpdateEvent>("lios-task-updated", (event) => listener(event.payload));
}

export function createTaskApi(
  invokeTask: InvokeTask = invoke,
  subscribeTasks: SubscribeTasks = subscribeToTauriTasks
): TaskApi {
  return {
    listTasks: () => invokeTask<TaskSummary[]>("list_tasks"),
    getTask: (taskId) => invokeTask<TaskSummary | null>("get_task", { taskId }),
    listTaskItems: (taskId, offset, limit) =>
      invokeTask<TaskItemsPage>("list_task_items", { taskId, offset, limit }),
    runAction: (action, taskId) => invokeTask<void>(actionCommands[action], { taskId }),
    subscribe: subscribeTasks
  };
}
