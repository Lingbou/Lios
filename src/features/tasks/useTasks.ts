import { useCallback, useEffect, useRef, useState } from "react";
import type { TaskAction, TaskApi, TaskSummary } from "./taskTypes.ts";

type UseTasksOptions = {
  api: TaskApi;
  initialTasks?: TaskSummary[];
  pollingIntervalMs?: number;
  onError: (error: unknown) => void;
};

type ListRequest = {
  api: TaskApi;
  revision: number;
  mutationEpoch: number;
  sequence: number;
};

type MutationRequest = {
  api: TaskApi;
  action: TaskAction;
  taskId: string;
  baselineTask?: TaskSummary;
};

function mergeTasks(
  current: TaskSummary[],
  incoming: TaskSummary[],
  { acceptEqual, keepMissing }: { acceptEqual: boolean; keepMissing: boolean }
) {
  const currentById = new Map(current.map((task) => [task.id, task]));
  const incomingIds = new Set<string>();
  const merged = incoming.map((nextTask) => {
    incomingIds.add(nextTask.id);
    const existing = currentById.get(nextTask.id);
    if (!existing) return nextTask;
    if (nextTask.updated_at > existing.updated_at) return nextTask;
    if (acceptEqual && nextTask.updated_at === existing.updated_at) return nextTask;
    return existing;
  });
  if (!keepMissing) return merged;
  return [...merged, ...current.filter((task) => !incomingIds.has(task.id))];
}

function sameTaskSummary(left: TaskSummary | undefined, right: TaskSummary | undefined) {
  return Boolean(
    left &&
      right &&
      left.id === right.id &&
      left.account_id === right.account_id &&
      left.space_id === right.space_id &&
      left.state === right.state &&
      left.label === right.label &&
      left.phase === right.phase &&
      left.progress_total === right.progress_total &&
      left.progress_done === right.progress_done &&
      left.bytes_total === right.bytes_total &&
      left.bytes_done === right.bytes_done &&
      left.speed_bps === right.speed_bps &&
      left.eta_seconds === right.eta_seconds &&
      left.attempt === right.attempt &&
      left.created_at === right.created_at &&
      left.updated_at === right.updated_at &&
      left.error === right.error &&
      left.item_count === right.item_count &&
      left.can_retry === right.can_retry
  );
}

export function useTasks({
  api,
  initialTasks,
  pollingIntervalMs = 1000,
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
  const acceptedSnapshotRevision = useRef(initialTasks === undefined ? 0 : 1);
  const latestListRequestSequence = useRef(0);
  const mutationEpoch = useRef(0);
  const activeMutation = useRef<{ api: TaskApi; count: number } | null>(null);
  const removedTaskIds = useRef<Set<string>>(new Set());
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
    acceptedSnapshotRevision.current += 1;
    latestListRequestSequence.current += 1;
    mutationEpoch.current += 1;
    activeMutation.current = null;
    removedTaskIds.current.clear();
    setTasks(nextTasks);
    setReady(nextReady);
    setPendingActions({});
  }, [api, initialTasks]);

  const isCurrentApi = useCallback(
    (requestApi: TaskApi) => mountedRef.current && apiRef.current === requestApi,
    []
  );

  const commitTasks = useCallback((nextTasks: TaskSummary[], completeSnapshot: boolean) => {
    if (!mountedRef.current) return false;
    acceptedSnapshotRevision.current += 1;
    tasksRef.current = nextTasks;
    setTasks(nextTasks);
    if (completeSnapshot && !readyRef.current) {
      readyRef.current = true;
      setReady(true);
    }
    return true;
  }, []);

  const acceptServerSnapshot = useCallback(
    (nextTasks: TaskSummary[], keepMissing: boolean) => {
      const visibleTasks = nextTasks.filter((task) => !removedTaskIds.current.has(task.id));
      const incomingIds = new Set(visibleTasks.map((task) => task.id));
      if (!keepMissing) {
        for (const task of tasksRef.current) {
          if (!incomingIds.has(task.id)) removedTaskIds.current.add(task.id);
        }
      }
      return commitTasks(
        mergeTasks(tasksRef.current, visibleTasks, { acceptEqual: true, keepMissing }),
        true
      );
    },
    [commitTasks]
  );

  const acceptSetupSnapshot = useCallback(
    (nextTasks: TaskSummary[]) =>
      commitTasks(
        mergeTasks(
          tasksRef.current,
          nextTasks.filter((task) => !removedTaskIds.current.has(task.id)),
          { acceptEqual: false, keepMissing: true }
        ),
        true
      ),
    [commitTasks]
  );

  const beginListRequest = useCallback(
    (requestApi: TaskApi): ListRequest => ({
      api: requestApi,
      revision: acceptedSnapshotRevision.current,
      mutationEpoch: mutationEpoch.current,
      sequence: ++latestListRequestSequence.current
    }),
    []
  );

  const acceptListResponse = useCallback(
    (request: ListRequest, nextTasks: TaskSummary[]) => {
      const mutation = activeMutation.current;
      if (
        !isCurrentApi(request.api) ||
        (mutation?.api === request.api && mutation.count > 0) ||
        acceptedSnapshotRevision.current !== request.revision ||
        mutationEpoch.current !== request.mutationEpoch ||
        latestListRequestSequence.current !== request.sequence
      ) {
        return false;
      }
      return acceptServerSnapshot(nextTasks, false);
    },
    [acceptServerSnapshot, isCurrentApi]
  );

  const beginMutationRequest = useCallback(
    (requestApi: TaskApi, action: TaskAction, taskId: string): MutationRequest => {
      const currentMutation = activeMutation.current;
      activeMutation.current = {
        api: requestApi,
        count: currentMutation?.api === requestApi ? currentMutation.count + 1 : 1
      };
      mutationEpoch.current += 1;
      return {
        api: requestApi,
        action,
        taskId,
        baselineTask: tasksRef.current.find((task) => task.id === taskId)
      };
    },
    []
  );

  const finishMutationRequest = useCallback((request: MutationRequest) => {
    const currentMutation = activeMutation.current;
    if (currentMutation?.api === request.api) {
      activeMutation.current =
        currentMutation.count > 1
          ? { api: request.api, count: currentMutation.count - 1 }
          : null;
    }
    if (apiRef.current === request.api) mutationEpoch.current += 1;
  }, []);

  const acceptMutationResponse = useCallback(
    (request: MutationRequest, nextTasks: TaskSummary[]) => {
      if (!isCurrentApi(request.api)) return false;

      const clearsTarget =
        request.action === "clear" && !nextTasks.some((task) => task.id === request.taskId);
      if (clearsTarget) removedTaskIds.current.add(request.taskId);

      const visibleTasks = nextTasks.filter((task) => !removedTaskIds.current.has(task.id));
      const current = clearsTarget
        ? tasksRef.current.filter((task) => task.id !== request.taskId)
        : tasksRef.current;
      let merged = mergeTasks(current, visibleTasks, { acceptEqual: false, keepMissing: true });
      const responseTarget = visibleTasks.find((task) => task.id === request.taskId);
      const currentTarget = current.find((task) => task.id === request.taskId);
      if (
        responseTarget &&
        currentTarget &&
        sameTaskSummary(currentTarget, request.baselineTask) &&
        responseTarget.updated_at === currentTarget.updated_at
      ) {
        merged = merged.map((task) => (task.id === request.taskId ? responseTarget : task));
      }
      return commitTasks(merged, true);
    },
    [commitTasks, isCurrentApi]
  );

  const syncTasks = useCallback(
    (nextTasks: TaskSummary[]) => {
      if (!isCurrentApi(api)) return false;
      return acceptSetupSnapshot(nextTasks);
    },
    [acceptSetupSnapshot, api, isCurrentApi]
  );

  const upsertTask = useCallback(
    (nextTask: TaskSummary) => {
      if (!isCurrentApi(api)) return false;
      if (removedTaskIds.current.has(nextTask.id)) return false;
      const current = tasksRef.current;
      const existing = current.find((task) => task.id === nextTask.id);
      if (existing && nextTask.updated_at <= existing.updated_at) return false;
      return commitTasks(
        [nextTask, ...current.filter((task) => task.id !== nextTask.id)],
        false
      );
    },
    [api, commitTasks, isCurrentApi]
  );

  const refreshTasks = useCallback(async () => {
    if (!isCurrentApi(api)) return tasksRef.current;
    const request = beginListRequest(api);
    const nextTasks = await api.listTasks();
    acceptListResponse(request, nextTasks);
    return nextTasks;
  }, [acceptListResponse, api, beginListRequest, isCurrentApi]);

  const onAction = useCallback(
    async (action: TaskAction, taskId: string) => {
      if (!isCurrentApi(api)) return;
      const request = beginMutationRequest(api, action, taskId);
      setPendingActions((current) => ({ ...current, [taskId]: action }));
      try {
        const nextTasks = await api.runAction(action, taskId);
        acceptMutationResponse(request, nextTasks);
      } catch (error) {
        if (isCurrentApi(api)) onError(error);
      } finally {
        finishMutationRequest(request);
        if (isCurrentApi(request.api)) {
          setPendingActions((current) => {
            const next = { ...current };
            delete next[taskId];
            return next;
          });
        }
      }
    },
    [
      acceptMutationResponse,
      api,
      beginMutationRequest,
      finishMutationRequest,
      isCurrentApi,
      onError
    ]
  );

  useEffect(() => {
    let active = true;
    let removeListener: (() => void) | undefined;
    api
      .subscribe((nextTasks) => {
        if (active && isCurrentApi(api)) acceptServerSnapshot(nextTasks, true);
      })
      .then((unlisten) => {
        if (active) removeListener = unlisten;
        else unlisten();
      })
      .catch(() => undefined);

    const pull = async () => {
      if (!active || !isCurrentApi(api)) return;
      const request = beginListRequest(api);
      try {
        const nextTasks = await api.listTasks();
        if (active) acceptListResponse(request, nextTasks);
      } catch {
        // Setup and explicit actions own user-facing errors; polling remains a quiet fallback.
      } finally {
        if (active && isCurrentApi(api) && pollingIntervalMs > 0) {
          timer = window.setTimeout(pull, pollingIntervalMs);
        }
      }
    };

    let timer: number | undefined;
    if (pollingIntervalMs > 0) void pull();
    return () => {
      active = false;
      removeListener?.();
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [
    acceptListResponse,
    acceptServerSnapshot,
    api,
    beginListRequest,
    isCurrentApi,
    pollingIntervalMs
  ]);

  return {
    tasks,
    ready,
    pendingActions,
    onAction,
    listTaskItems: api.listTaskItems,
    refreshTasks,
    syncTasks,
    upsertTask
  };
}
