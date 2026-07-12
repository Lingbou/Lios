import { useVirtualizer } from "@tanstack/react-virtual";
import {
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  File,
  LoaderCircle,
  Pause,
  PauseCircle,
  Play,
  RefreshCw,
  RotateCcw,
  Trash2,
  XCircle
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  taskActionsForTask,
  taskItemProgressPercent,
  taskItemProgressText,
  taskItemStatusText,
  taskLabelText,
  taskProgressPercent,
  taskProgressText,
  taskStatusText
} from "./taskPresentation.ts";
import type { TaskAction, TaskItem, TaskItemsPage, TaskState, TaskSummary } from "./taskTypes.ts";

const PAGE_SIZE = 50;
const DETAIL_ROW_HEIGHT = 56;
const DETAIL_VIEWPORT_HEIGHT = 240;
const ACTIVE_DETAIL_REFRESH_MS = 5000;

const activeStates = new Set<TaskState>(["Queued", "Preparing", "Running", "Retrying", "Committing"]);
const terminalStates = new Set<TaskState>(["Failed", "Completed", "Canceled"]);
const runningStates = new Set<TaskState>(["Preparing", "Running", "Retrying", "Committing"]);

const actionDetails: Record<
  TaskAction,
  { label: string; className?: string; icon: typeof Pause }
> = {
  pause: { label: "暂停任务", icon: Pause },
  resume: { label: "继续任务", icon: Play },
  retry: { label: "重试任务", icon: RotateCcw },
  cancel: { label: "取消任务", className: "iconDanger", icon: XCircle },
  clear: { label: "清除记录", icon: Trash2 }
};

export type TaskCenterProps = {
  tasks: TaskSummary[];
  pendingActions: Partial<Record<string, TaskAction>>;
  onAction: (action: TaskAction, taskId: string) => Promise<void>;
  listTaskItems: (taskId: string, offset: number, limit: number) => Promise<TaskItemsPage>;
  onError?: (error: unknown) => void;
};

function TaskStateIcon({ state }: { state: TaskState }) {
  if (state === "Completed") return <CheckCircle2 aria-hidden />;
  if (state === "Failed" || state === "Canceled") return <XCircle aria-hidden />;
  if (state === "Paused") return <PauseCircle aria-hidden />;
  return <LoaderCircle aria-hidden />;
}

function TaskActionButtons({
  task,
  pendingAction,
  onAction
}: {
  task: TaskSummary;
  pendingAction?: TaskAction;
  onAction: TaskCenterProps["onAction"];
}) {
  return taskActionsForTask(task).map((action) => {
    const details = actionDetails[action];
    const Icon = details.icon;
    return (
      <button
        className={details.className}
        disabled={pendingAction !== undefined}
        key={action}
        title={details.label}
        aria-label={details.label}
        onClick={() => void onAction(action, task.id)}
      >
        {pendingAction === action ? <RefreshCw className="taskActionSpinner" aria-hidden /> : <Icon aria-hidden />}
      </button>
    );
  });
}

function TaskDetails({
  task,
  listTaskItems,
  onError
}: Pick<TaskCenterProps, "listTaskItems" | "onError"> & { task: TaskSummary }) {
  const viewportRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLDivElement>(null);
  const pagesRef = useRef<Map<number, TaskItem[]>>(new Map());
  const loadingPages = useRef<Set<number>>(new Set());
  const pendingForcedRefreshPages = useRef<Set<number>>(new Set());
  const visiblePages = useRef<number[]>([]);
  const totalRef = useRef(task.item_count);
  const listTaskItemsRef = useRef(listTaskItems);
  const onErrorRef = useRef(onError);
  const previousTaskState = useRef(task.state);
  const mountedRef = useRef(true);
  const [pages, setPages] = useState<Map<number, TaskItem[]>>(new Map());
  const [total, setTotal] = useState(task.item_count);
  const isActiveTask = activeStates.has(task.state);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      pendingForcedRefreshPages.current.clear();
    };
  }, []);

  useEffect(() => {
    totalRef.current = task.item_count;
    setTotal(task.item_count);
  }, [task.item_count]);

  useEffect(() => {
    listTaskItemsRef.current = listTaskItems;
  }, [listTaskItems]);

  useEffect(() => {
    onErrorRef.current = onError;
  }, [onError]);

  const loadPage = useCallback(
    async function loadTaskPage(pageIndex: number, refresh = false) {
      const offset = pageIndex * PAGE_SIZE;
      const currentTotal = totalRef.current;
      if (offset >= currentTotal) return;
      if (loadingPages.current.has(pageIndex)) {
        if (refresh && mountedRef.current) pendingForcedRefreshPages.current.add(pageIndex);
        return;
      }
      if (!refresh && pagesRef.current.has(pageIndex)) return;
      loadingPages.current.add(pageIndex);
      try {
        const page = await listTaskItemsRef.current(
          task.id,
          offset,
          Math.min(PAGE_SIZE, currentTotal - offset)
        );
        if (!mountedRef.current) return;
        totalRef.current = page.total;
        setTotal(page.total);
        const next = new Map(pagesRef.current);
        next.set(pageIndex, page.items);
        pagesRef.current = next;
        setPages(next);
      } catch (error) {
        if (mountedRef.current) onErrorRef.current?.(error);
      } finally {
        loadingPages.current.delete(pageIndex);
        if (
          mountedRef.current &&
          pendingForcedRefreshPages.current.delete(pageIndex)
        ) {
          void loadTaskPage(pageIndex, true);
        }
      }
    },
    [task.id]
  );

  const rowVirtualizer = useVirtualizer({
    count: total,
    getScrollElement: () => viewportRef.current,
    estimateSize: () => DETAIL_ROW_HEIGHT,
    overscan: 5,
    initialRect: { width: 800, height: DETAIL_VIEWPORT_HEIGHT },
    observeElementRect: (instance, callback) => {
      const element = instance.scrollElement;
      if (!element) return;
      const update = () =>
        callback({
          width: element.clientWidth || 800,
          height: element.clientHeight || DETAIL_VIEWPORT_HEIGHT
        });
      update();
      const observer = new ResizeObserver(update);
      observer.observe(element);
      return () => observer.disconnect();
    },
    observeElementOffset: (instance, callback) => {
      const element = instance.scrollElement;
      if (!element) return;
      const update = () => callback(element.scrollTop, false);
      update();
      element.addEventListener("scroll", update, { passive: true });
      return () => element.removeEventListener("scroll", update);
    }
  });
  const virtualRows = rowVirtualizer.getVirtualItems();
  const virtualKey = virtualRows.map((row) => `${row.index}:${row.start}`).join("|");

  useEffect(() => {
    canvasRef.current?.style.setProperty("--task-virtual-size", `${rowVirtualizer.getTotalSize()}px`);
  }, [rowVirtualizer, total, virtualKey]);

  useEffect(() => {
    const nextVisiblePages = Array.from(
      new Set(virtualRows.map((row) => Math.floor(row.index / PAGE_SIZE)))
    );
    visiblePages.current = nextVisiblePages;
    for (const pageIndex of nextVisiblePages) void loadPage(pageIndex);
  }, [loadPage, virtualKey]);

  useEffect(() => {
    if (!isActiveTask) return;
    const timer = window.setInterval(() => {
      for (const pageIndex of visiblePages.current) void loadPage(pageIndex, true);
    }, ACTIVE_DETAIL_REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [isActiveTask, loadPage]);

  useEffect(() => {
    const previousState = previousTaskState.current;
    previousTaskState.current = task.state;
    if (terminalStates.has(previousState) || !terminalStates.has(task.state)) return;
    for (const pageIndex of visiblePages.current) void loadPage(pageIndex, true);
  }, [loadPage, task.state]);

  return (
    <div
      className="taskDetailViewport"
      ref={viewportRef}
      aria-label={`${taskLabelText(task.label)}文件明细`}
    >
      <div className="taskVirtualCanvas" ref={canvasRef}>
        {virtualRows.map((virtualRow) => {
          const pageIndex = Math.floor(virtualRow.index / PAGE_SIZE);
          const item = pages.get(pageIndex)?.[virtualRow.index % PAGE_SIZE];
          return (
            <div
              className={`taskVirtualRow ${item?.state.toLowerCase() ?? "loading"}`}
              data-index={virtualRow.index}
              key={virtualRow.key}
              ref={(node) => {
                node?.style.setProperty("--task-row-start", `${virtualRow.start}px`);
              }}
            >
              {item ? (
                <>
                  <File aria-hidden />
                  <div className="taskFileInfo">
                    <strong title={item.relative_path || item.name}>
                      {(item.relative_path || item.name).replace(/\\/g, "/")}
                    </strong>
                    <span>{taskItemStatusText(item)}</span>
                  </div>
                  <div className="taskFileProgress">
                    <progress
                      className="meter"
                      max={100}
                      value={taskItemProgressPercent(item)}
                      aria-label={`${taskItemProgressPercent(item)}%`}
                    >
                      {taskItemProgressPercent(item)}%
                    </progress>
                    <small>{taskItemProgressText(item)}</small>
                  </div>
                </>
              ) : (
                <div className="taskFileLoading">正在载入文件状态…</div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

export function TaskCenter({
  tasks,
  pendingActions,
  onAction,
  listTaskItems,
  onError
}: TaskCenterProps) {
  const [expandedTaskIds, setExpandedTaskIds] = useState<Set<string>>(new Set());
  const counts = useMemo(
    () => ({
      running: tasks.filter((task) => runningStates.has(task.state)).length,
      failed: tasks.filter((task) => task.state === "Failed").length,
      total: tasks.length
    }),
    [tasks]
  );

  const toggleExpanded = (taskId: string) => {
    setExpandedTaskIds((current) => {
      const next = new Set(current);
      if (next.has(taskId)) next.delete(taskId);
      else next.add(taskId);
      return next;
    });
  };

  return (
    <section className="taskDrawer taskCenter" aria-label="任务">
      <div className="taskHeader">
        <div>
          <span>任务</span>
          <small>传输中心</small>
        </div>
        <small>{counts.running} 运行 / {counts.failed} 失败 / {counts.total} 记录</small>
      </div>
      <div className="taskRows">
        {tasks.length === 0 ? (
          <div className="taskEmpty">暂无任务</div>
        ) : (
          tasks.map((task) => {
            const expanded = expandedTaskIds.has(task.id);
            const progress = taskProgressPercent(task);
            return (
              <article className={`taskRow ${task.state.toLowerCase()}`} key={task.id}>
                <div className="taskSummary">
                  <button
                    className="taskExpandButton"
                    disabled={task.item_count === 0}
                    title={expanded ? "收起任务详情" : "展开任务详情"}
                    aria-label={expanded ? "收起任务详情" : "展开任务详情"}
                    aria-expanded={expanded}
                    onClick={() => toggleExpanded(task.id)}
                  >
                    {expanded ? <ChevronDown aria-hidden /> : <ChevronRight aria-hidden />}
                  </button>
                  <div className="taskIcon"><TaskStateIcon state={task.state} /></div>
                  <div className="taskInfo">
                    <strong>{taskLabelText(task.label)}<small>{task.item_count} 个文件</small></strong>
                    <span>{taskStatusText(task)}</span>
                  </div>
                  <div className="taskProgress">
                    <progress className="meter" max={100} value={progress} aria-label={`${progress}%`}>
                      {progress}%
                    </progress>
                    <small>{taskProgressText(task)}</small>
                  </div>
                  <div className="taskButtons">
                    <TaskActionButtons
                      task={task}
                      pendingAction={pendingActions[task.id]}
                      onAction={onAction}
                    />
                  </div>
                </div>
                {expanded && (
                  <TaskDetails task={task} listTaskItems={listTaskItems} onError={onError} />
                )}
              </article>
            );
          })
        )}
      </div>
    </section>
  );
}
