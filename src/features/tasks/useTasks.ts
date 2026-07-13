import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  TaskAction,
  TaskApi,
  TaskSummary,
  TaskUpdateEvent
} from "./taskTypes.ts";

type UseTasksOptions = {
  api: TaskApi;
  initialTasks?: TaskSummary[];
  fallbackAfterMs?: number;
  onError: (error: unknown) => void;
};

type ListRequest = {
  api: TaskApi;
  revision: number;
  sequence: number;
};

const activeStates = new Set([
  "Queued",
  "Preparing",
  "Running",
  "Retrying",
  "Committing"
]);

function isActiveTask(task: TaskSummary) {
  return activeStates.has(task.state);
}

export function useTasks({
  api,
  initialTasks,
  fallbackAfterMs = 10_000,
  onError
}: UseTasksOptions) {
  const initialSnapshot = initialTasks ?? [];
  const [tasks, setTasks] = useState<TaskSummary[]>(initialSnapshot);
  const [ready, setReady] = useState(initialTasks !== undefined);
  const [pendingActions, setPendingActions] = useState<Partial<Record<string, TaskAction>>>({});
  const tasksRef = useRef(initialSnapshot);
  const readyRef = useRef(initialTasks !== undefined);
  const mountedRef = useRef(true);
  const apiRef = useRef(api);
  const generationApiRef = useRef(api);
  const revisionRef = useRef(initialTasks === undefined ? 0 : 1);
  const latestListSequenceRef = useRef(0);
  const removedTaskIdsRef = useRef(new Set<string>());
  const activeHeartbeatRef = useRef(new Map<string, number>());
  apiRef.current = api;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    if (generationApiRef.current === api) return;
    generationApiRef.current = api;
    const nextTasks = initialTasks ?? [];
    const nextReady = initialTasks !== undefined;
    tasksRef.current = nextTasks;
    readyRef.current = nextReady;
    revisionRef.current += 1;
    latestListSequenceRef.current += 1;
    removedTaskIdsRef.current.clear();
    activeHeartbeatRef.current.clear();
    setTasks(nextTasks);
    setReady(nextReady);
    setPendingActions({});
  }, [api, initialTasks]);

  const isCurrentApi = useCallback(
    (requestApi: TaskApi) => mountedRef.current && apiRef.current === requestApi,
    []
  );

  const syncActiveHeartbeats = useCallback((nextTasks: TaskSummary[], now = Date.now()) => {
    const activeIds = new Set(
      nextTasks.filter(isActiveTask).map((task) => task.id)
    );
    for (const taskId of activeIds) {
      if (!activeHeartbeatRef.current.has(taskId)) {
        activeHeartbeatRef.current.set(taskId, now);
      }
    }
    for (const taskId of activeHeartbeatRef.current.keys()) {
      if (!activeIds.has(taskId)) activeHeartbeatRef.current.delete(taskId);
    }
  }, []);

  const commitTasks = useCallback(
    (nextTasks: TaskSummary[], completeSnapshot: boolean) => {
      if (!mountedRef.current) return false;
      revisionRef.current += 1;
      tasksRef.current = nextTasks;
      syncActiveHeartbeats(nextTasks);
      setTasks(nextTasks);
      if (completeSnapshot && !readyRef.current) {
        readyRef.current = true;
        setReady(true);
      }
      return true;
    },
    [syncActiveHeartbeats]
  );

  const beginListRequest = useCallback(
    (requestApi: TaskApi): ListRequest => ({
      api: requestApi,
      revision: revisionRef.current,
      sequence: ++latestListSequenceRef.current
    }),
    []
  );

  const acceptListResponse = useCallback(
    (request: ListRequest, nextTasks: TaskSummary[]) => {
      if (
        !isCurrentApi(request.api) ||
        request.revision !== revisionRef.current ||
        request.sequence !== latestListSequenceRef.current
      ) {
        return false;
      }
      const visible = nextTasks.filter(
        (task) => !removedTaskIdsRef.current.has(task.id)
      );
      const incomingIds = new Set(visible.map((task) => task.id));
      for (const task of tasksRef.current) {
        if (!incomingIds.has(task.id)) removedTaskIdsRef.current.add(task.id);
      }
      return commitTasks(visible, true);
    },
    [commitTasks, isCurrentApi]
  );

  const upsertTaskForApi = useCallback(
    (requestApi: TaskApi, nextTask: TaskSummary, acceptEqual: boolean) => {
      if (!isCurrentApi(requestApi)) return false;
      if (removedTaskIdsRef.current.has(nextTask.id)) return false;
      const current = tasksRef.current;
      const existing = current.find((task) => task.id === nextTask.id);
      if (
        existing &&
        (nextTask.updated_at < existing.updated_at ||
          (!acceptEqual && nextTask.updated_at === existing.updated_at))
      ) {
        return false;
      }
      return commitTasks(
        [nextTask, ...current.filter((task) => task.id !== nextTask.id)],
        false
      );
    },
    [commitTasks, isCurrentApi]
  );

  const removeTasksForApi = useCallback(
    (requestApi: TaskApi, taskIds: string[]) => {
      if (!isCurrentApi(requestApi) || taskIds.length === 0) return false;
      const removedIds = new Set(taskIds);
      for (const taskId of removedIds) {
        removedTaskIdsRef.current.add(taskId);
        activeHeartbeatRef.current.delete(taskId);
      }
      const nextTasks = tasksRef.current.filter(
        (task) => !removedIds.has(task.id)
      );
      if (nextTasks.length === tasksRef.current.length) {
        revisionRef.current += 1;
        return false;
      }
      return commitTasks(nextTasks, false);
    },
    [commitTasks, isCurrentApi]
  );

  const acceptEvent = useCallback(
    (requestApi: TaskApi, event: TaskUpdateEvent) => {
      if (!isCurrentApi(requestApi)) return false;
      const now = Date.now();
      if (event.kind === "remove") {
        return removeTasksForApi(requestApi, event.task_ids);
      }
      if (isActiveTask(event.task)) {
        activeHeartbeatRef.current.set(event.task.id, now);
      } else {
        activeHeartbeatRef.current.delete(event.task.id);
      }
      return upsertTaskForApi(requestApi, event.task, false);
    },
    [isCurrentApi, removeTasksForApi, upsertTaskForApi]
  );

  const refreshTasks = useCallback(async () => {
    if (!isCurrentApi(api)) return tasksRef.current;
    const request = beginListRequest(api);
    const nextTasks = await api.listTasks();
    acceptListResponse(request, nextTasks);
    return tasksRef.current;
  }, [acceptListResponse, api, beginListRequest, isCurrentApi]);

  const onAction = useCallback(
    async (action: TaskAction, taskId: string) => {
      if (!isCurrentApi(api)) return;
      setPendingActions((current) => ({ ...current, [taskId]: action }));
      try {
        await api.runAction(action, taskId);
        const refreshed = await api.getTask(taskId);
        if (refreshed) upsertTaskForApi(api, refreshed, true);
        else removeTasksForApi(api, [taskId]);
      } catch (error) {
        if (isCurrentApi(api)) onError(error);
      } finally {
        if (isCurrentApi(api)) {
          setPendingActions((current) => {
            const next = { ...current };
            delete next[taskId];
            return next;
          });
        }
      }
    },
    [api, isCurrentApi, onError, removeTasksForApi, upsertTaskForApi]
  );

  const upsertTask = useCallback(
    (nextTask: TaskSummary) => upsertTaskForApi(api, nextTask, false),
    [api, upsertTaskForApi]
  );

  useEffect(() => {
    let active = true;
    let removeListener: (() => void) | undefined;
    const initialPull = async () => {
      try {
        await refreshTasks();
      } catch {
        // Setup owns user-facing initialization errors.
      }
    };

    api
      .subscribe((event) => {
        if (active) acceptEvent(api, event);
      })
      .then((unlisten) => {
        if (!active) {
          unlisten();
          return;
        }
        removeListener = unlisten;
        void initialPull();
      })
      .catch(() => {
        if (active) void initialPull();
      });

    return () => {
      active = false;
      removeListener?.();
    };
  }, [acceptEvent, api, refreshTasks]);

  const activeTaskKey = useMemo(
    () =>
      tasks
        .filter(isActiveTask)
        .map((task) => task.id)
        .sort()
        .join("|"),
    [tasks]
  );

  useEffect(() => {
    if (!ready || !activeTaskKey || fallbackAfterMs <= 0) return;
    let active = true;
    let timer: number | undefined;

    const schedule = () => {
      if (!active) return;
      const activeTasks = tasksRef.current.filter(isActiveTask);
      if (activeTasks.length === 0) return;
      const now = Date.now();
      const nextDue = Math.min(
        ...activeTasks.map(
          (task) =>
            (activeHeartbeatRef.current.get(task.id) ?? now) + fallbackAfterMs
        )
      );
      timer = window.setTimeout(check, Math.max(0, nextDue - now));
    };

    const check = async () => {
      if (!active) return;
      const now = Date.now();
      const activeTasks = tasksRef.current.filter(isActiveTask);
      const stale = activeTasks.some(
        (task) =>
          now - (activeHeartbeatRef.current.get(task.id) ?? now) >=
          fallbackAfterMs
      );
      if (stale) {
        try {
          await refreshTasks();
        } catch {
          // Events remain primary; fallback failures stay quiet.
        }
        const checkedAt = Date.now();
        for (const task of tasksRef.current.filter(isActiveTask)) {
          activeHeartbeatRef.current.set(task.id, checkedAt);
        }
      }
      schedule();
    };

    schedule();
    return () => {
      active = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [activeTaskKey, fallbackAfterMs, ready, refreshTasks]);

  return {
    tasks,
    ready,
    pendingActions,
    onAction,
    listTaskItems: api.listTaskItems,
    refreshTasks,
    upsertTask
  };
}
