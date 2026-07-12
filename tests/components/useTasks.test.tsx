import { act, renderHook } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import { StrictMode } from "react";
import { useTasks } from "../../src/features/tasks/useTasks.ts";
import type { TaskApi, TaskSummary } from "../../src/features/tasks/taskTypes.ts";

function summary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-a",
    account_id: "account",
    space_id: "space",
    state: "Queued",
    label: "upload",
    phase: null,
    progress_total: 1,
    progress_done: 0,
    bytes_total: 1,
    bytes_done: 0,
    speed_bps: 0,
    eta_seconds: null,
    attempt: 0,
    created_at: "2026-07-12T00:00:00Z",
    updated_at: "2026-07-12T00:00:00Z",
    error: null,
    item_count: 1,
    can_retry: false,
    ...overrides
  };
}

function taskApi(): TaskApi {
  return {
    listTasks: vi.fn(async () => []),
    listTaskItems: vi.fn(async (taskId, offset) => ({
      task_id: taskId,
      offset,
      total: 0,
      items: []
    })),
    runAction: vi.fn(async () => []),
    subscribe: vi.fn(async () => () => undefined)
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

function renderTasks(initialTasks: TaskSummary[]) {
  const api = taskApi();
  return renderHook(() =>
    useTasks({ api, initialTasks, pollingIntervalMs: 0, onError: vi.fn() })
  );
}

test("an older enqueue summary cannot replace a newer task state", () => {
  const running = summary({
    state: "Running",
    updated_at: "2026-07-12T00:00:02Z"
  });
  const { result } = renderTasks([running]);

  act(() => {
    result.current.upsertTask(
      summary({ state: "Queued", updated_at: "2026-07-12T00:00:01Z" })
    );
  });

  expect(result.current.tasks).toEqual([running]);
});

test("an equal timestamp preserves the existing task state", () => {
  const existing = summary({ state: "Running", updated_at: "2026-07-12T00:00:02Z" });
  const incoming = summary({ state: "Queued", updated_at: "2026-07-12T00:00:02Z" });
  const { result } = renderTasks([existing]);

  act(() => {
    result.current.upsertTask(incoming);
  });

  expect(result.current.tasks).toEqual([existing]);
});

test("a newer timestamp replaces the existing summary and moves it first", () => {
  const other = summary({ id: "task-b", updated_at: "2026-07-12T00:00:04Z" });
  const existing = summary({ state: "Running", updated_at: "2026-07-12T00:00:02Z" });
  const replacement = summary({ state: "Completed", updated_at: "2026-07-12T00:00:03Z" });
  const { result } = renderTasks([other, existing]);

  act(() => {
    result.current.upsertTask(replacement);
  });

  expect(result.current.tasks).toEqual([replacement, other]);
});

test("a summary for a new task inserts at the front", () => {
  const existing = summary({ id: "task-b", state: "Running" });
  const incoming = summary({ id: "task-a" });
  const { result } = renderTasks([existing]);

  act(() => {
    result.current.upsertTask(incoming);
  });

  expect(result.current.tasks).toEqual([incoming, existing]);
});

test("an event received during polling cannot be overwritten by the stale poll response", async () => {
  const poll = deferred<TaskSummary[]>();
  const running = summary({
    state: "Running",
    updated_at: "2026-07-12T00:00:01Z"
  });
  const completed = summary({
    state: "Completed",
    updated_at: "2026-07-12T00:00:02Z"
  });
  let listener: ((tasks: TaskSummary[]) => void) | undefined;
  const api = taskApi();
  api.listTasks = vi.fn(() => poll.promise);
  api.subscribe = vi.fn(async (nextListener) => {
    listener = nextListener;
    return () => undefined;
  });

  const { result, unmount } = renderHook(() =>
    useTasks({ api, pollingIntervalMs: 60_000, onError: vi.fn() })
  );

  expect(api.listTasks).toHaveBeenCalledTimes(1);
  act(() => listener?.([completed]));
  expect(result.current.tasks).toEqual([completed]);

  await act(async () => {
    poll.resolve([running]);
    await poll.promise;
  });

  expect(result.current.tasks).toEqual([completed]);
  unmount();
});

test("a poll started during an action cannot supersede the action response", async () => {
  vi.useFakeTimers();
  const action = deferred<TaskSummary[]>();
  const running = summary({ state: "Running", updated_at: "2026-07-12T00:00:01Z" });
  const stale = summary({ state: "Running", updated_at: "2026-07-12T00:00:01Z" });
  const completed = summary({ state: "Completed", updated_at: "2026-07-12T00:00:02Z" });
  const api = taskApi();
  api.listTasks = vi
    .fn<() => Promise<TaskSummary[]>>()
    .mockResolvedValueOnce([running])
    .mockResolvedValueOnce([stale]);
  api.runAction = vi.fn(() => action.promise);
  let unmount: (() => void) | undefined;
  let actionRequest: Promise<void> | undefined;

  try {
    const hook = renderHook(() =>
      useTasks({ api, initialTasks: [running], pollingIntervalMs: 1000, onError: vi.fn() })
    );
    unmount = hook.unmount;

    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(api.listTasks).toHaveBeenCalledTimes(1);

    act(() => {
      actionRequest = hook.result.current.onAction("pause", running.id);
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(api.listTasks).toHaveBeenCalledTimes(2);

    await act(async () => {
      action.resolve([completed]);
      await actionRequest;
    });

    expect(hook.result.current.tasks).toEqual([completed]);
  } finally {
    action.resolve([completed]);
    if (actionRequest) await actionRequest;
    unmount?.();
    vi.clearAllTimers();
    vi.useRealTimers();
  }
});

test("a stale setup sync cannot roll back a newer task event", () => {
  const running = summary({ state: "Running", updated_at: "2026-07-12T00:00:01Z" });
  const completed = summary({ state: "Completed", updated_at: "2026-07-12T00:00:02Z" });
  let listener: ((tasks: TaskSummary[]) => void) | undefined;
  const api = taskApi();
  api.subscribe = vi.fn(async (nextListener) => {
    listener = nextListener;
    return () => undefined;
  });
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [running], pollingIntervalMs: 0, onError: vi.fn() })
  );

  act(() => listener?.([completed]));
  act(() => result.current.syncTasks([running]));

  expect(result.current.tasks).toEqual([completed]);
});

test("an equal-timestamp setup sync preserves the existing event state", () => {
  const running = summary({ state: "Running", updated_at: "2026-07-12T00:00:02Z" });
  const completed = summary({ state: "Completed", updated_at: "2026-07-12T00:00:02Z" });
  let listener: ((tasks: TaskSummary[]) => void) | undefined;
  const api = taskApi();
  api.subscribe = vi.fn(async (nextListener) => {
    listener = nextListener;
    return () => undefined;
  });
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [running], pollingIntervalMs: 0, onError: vi.fn() })
  );

  act(() => listener?.([completed]));
  act(() => result.current.syncTasks([running]));

  expect(result.current.tasks).toEqual([completed]);
});

test("a later event may advance state at the same server timestamp", () => {
  const running = summary({ state: "Running", updated_at: "2026-07-12T00:00:02Z" });
  const completed = summary({ state: "Completed", updated_at: "2026-07-12T00:00:02Z" });
  let listener: ((tasks: TaskSummary[]) => void) | undefined;
  const api = taskApi();
  api.subscribe = vi.fn(async (nextListener) => {
    listener = nextListener;
    return () => undefined;
  });
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [running], pollingIntervalMs: 0, onError: vi.fn() })
  );

  act(() => listener?.([completed]));

  expect(result.current.tasks).toEqual([completed]);
});

test("an accepted list response may advance state at the same server timestamp", async () => {
  const running = summary({ state: "Running", updated_at: "2026-07-12T00:00:02Z" });
  const completed = summary({ state: "Completed", updated_at: "2026-07-12T00:00:02Z" });
  const api = taskApi();
  api.listTasks = vi.fn(async () => [completed]);
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [running], pollingIntervalMs: 0, onError: vi.fn() })
  );

  await act(async () => {
    await result.current.refreshTasks();
  });

  expect(result.current.tasks).toEqual([completed]);
});

test("a clear action response removes tasks missing from its complete snapshot", async () => {
  const cleared = summary({ id: "task-a", state: "Completed" });
  const remaining = summary({ id: "task-b", state: "Running" });
  const api = taskApi();
  api.runAction = vi.fn(async () => [remaining]);
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [cleared, remaining], pollingIntervalMs: 0, onError: vi.fn() })
  );

  await act(async () => {
    await result.current.onAction("clear", cleared.id);
  });

  expect(result.current.tasks).toEqual([remaining]);
});

test("a clear action remains authoritative after an intervening task event", async () => {
  const action = deferred<TaskSummary[]>();
  const cleared = summary({ id: "task-a", state: "Completed" });
  const remaining = summary({ id: "task-b", state: "Running" });
  let listener: ((tasks: TaskSummary[]) => void) | undefined;
  const api = taskApi();
  api.runAction = vi.fn(() => action.promise);
  api.subscribe = vi.fn(async (nextListener) => {
    listener = nextListener;
    return () => undefined;
  });
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [cleared, remaining], pollingIntervalMs: 0, onError: vi.fn() })
  );
  let request: Promise<void>;

  act(() => {
    request = result.current.onAction("clear", cleared.id);
  });
  act(() => listener?.([cleared, remaining]));
  await act(async () => {
    action.resolve([remaining]);
    await request!;
  });

  expect(result.current.tasks).toEqual([remaining]);
});

test("an unchanged equal-timestamp event does not suppress the later action result", async () => {
  const action = deferred<TaskSummary[]>();
  const running = summary({ state: "Running", updated_at: "2026-07-12T00:00:02Z" });
  const paused = summary({ state: "Paused", updated_at: "2026-07-12T00:00:02Z" });
  let listener: ((tasks: TaskSummary[]) => void) | undefined;
  const api = taskApi();
  api.runAction = vi.fn(() => action.promise);
  api.subscribe = vi.fn(async (nextListener) => {
    listener = nextListener;
    return () => undefined;
  });
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [running], pollingIntervalMs: 0, onError: vi.fn() })
  );
  let request: Promise<void>;

  act(() => {
    request = result.current.onAction("pause", running.id);
  });
  act(() => listener?.([{ ...running }]));
  await act(async () => {
    action.resolve([paused]);
    await request!;
  });

  expect(result.current.tasks).toEqual([paused]);
});

test("concurrent actions on different tasks preserve both responses when they resolve out of order", async () => {
  const firstAction = deferred<TaskSummary[]>();
  const secondAction = deferred<TaskSummary[]>();
  const first = summary({
    id: "task-a",
    state: "Running",
    updated_at: "2026-07-12T00:00:01Z"
  });
  const second = summary({
    id: "task-b",
    state: "Running",
    updated_at: "2026-07-12T00:00:01Z"
  });
  const paused = summary({
    id: "task-a",
    state: "Paused",
    updated_at: "2026-07-12T00:00:02Z"
  });
  const canceled = summary({
    id: "task-b",
    state: "Canceled",
    updated_at: "2026-07-12T00:00:03Z"
  });
  const api = taskApi();
  api.runAction = vi.fn((_action, taskId) =>
    taskId === first.id ? firstAction.promise : secondAction.promise
  );
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [first, second], pollingIntervalMs: 0, onError: vi.fn() })
  );
  let firstRequest: Promise<void>;
  let secondRequest: Promise<void>;

  act(() => {
    firstRequest = result.current.onAction("pause", first.id);
    secondRequest = result.current.onAction("cancel", second.id);
  });
  await act(async () => {
    secondAction.resolve([first, canceled]);
    await secondRequest!;
    firstAction.resolve([paused, second]);
    await firstRequest!;
  });

  expect(result.current.tasks).toEqual([paused, canceled]);
});

test("a stale setup sync cannot resurrect a task removed by clear", async () => {
  const cleared = summary({ id: "task-a", state: "Completed" });
  const remaining = summary({ id: "task-b", state: "Running" });
  const api = taskApi();
  api.runAction = vi.fn(async () => [remaining]);
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [cleared, remaining], pollingIntervalMs: 0, onError: vi.fn() })
  );

  await act(async () => {
    await result.current.onAction("clear", cleared.id);
  });
  act(() => result.current.syncTasks([cleared, remaining]));

  expect(result.current.tasks).toEqual([remaining]);
});

test("a stale upsert cannot resurrect a task removed by clear", async () => {
  const cleared = summary({ id: "task-a", state: "Completed" });
  const remaining = summary({ id: "task-b", state: "Running" });
  const api = taskApi();
  api.runAction = vi.fn(async () => [remaining]);
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [cleared, remaining], pollingIntervalMs: 0, onError: vi.fn() })
  );

  await act(async () => {
    await result.current.onAction("clear", cleared.id);
  });
  act(() => result.current.upsertTask(cleared));

  expect(result.current.tasks).toEqual([remaining]);
});

test("a delayed server event cannot restore a cleared task", async () => {
  const cleared = summary({ id: "task-a", state: "Completed" });
  const restored = summary({ id: "task-a", state: "Running" });
  let listener: ((tasks: TaskSummary[]) => void) | undefined;
  const api = taskApi();
  api.runAction = vi.fn(async () => []);
  api.subscribe = vi.fn(async (nextListener) => {
    listener = nextListener;
    return () => undefined;
  });
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [cleared], pollingIntervalMs: 0, onError: vi.fn() })
  );

  await act(async () => {
    await result.current.onAction("clear", cleared.id);
  });
  act(() => listener?.([restored]));

  expect(result.current.tasks).toEqual([]);
});

test("a delayed action response cannot restore a task cleared by another action", async () => {
  const staleAction = deferred<TaskSummary[]>();
  const cleared = summary({ id: "task-a", state: "Completed" });
  const remaining = summary({
    id: "task-b",
    state: "Running",
    updated_at: "2026-07-12T00:00:01Z"
  });
  const canceled = summary({
    id: "task-b",
    state: "Canceled",
    updated_at: "2026-07-12T00:00:02Z"
  });
  const api = taskApi();
  api.runAction = vi.fn((action, taskId) => {
    if (action === "clear" && taskId === cleared.id) return Promise.resolve([remaining]);
    return staleAction.promise;
  });
  const { result } = renderHook(() =>
    useTasks({ api, initialTasks: [cleared, remaining], pollingIntervalMs: 0, onError: vi.fn() })
  );
  let delayedRequest: Promise<void>;

  act(() => {
    delayedRequest = result.current.onAction("cancel", remaining.id);
  });
  await act(async () => {
    await result.current.onAction("clear", cleared.id);
  });
  await act(async () => {
    staleAction.resolve([cleared, canceled]);
    await delayedRequest!;
  });

  expect(result.current.tasks).toEqual([canceled]);
});

test("ready requires a complete snapshot rather than a partial upsert", () => {
  const api = taskApi();
  const queued = summary();
  const { result } = renderHook(() =>
    useTasks({ api, pollingIntervalMs: 0, onError: vi.fn() })
  );

  expect(result.current.ready).toBe(false);
  act(() => result.current.upsertTask(queued));
  expect(result.current.ready).toBe(false);

  act(() => result.current.syncTasks([]));
  expect(result.current.ready).toBe(true);
  expect(result.current.tasks).toEqual([queued]);
});

test("a response from a previous API generation is ignored", async () => {
  const oldResponse = deferred<TaskSummary[]>();
  const current = summary({ state: "Running", updated_at: "2026-07-12T00:00:02Z" });
  const stale = summary({ state: "Queued", updated_at: "2026-07-12T00:00:01Z" });
  const firstApi = taskApi();
  firstApi.listTasks = vi.fn(() => oldResponse.promise);
  const secondApi = taskApi();
  const { result, rerender } = renderHook(
    ({ currentApi }: { currentApi: TaskApi }) =>
      useTasks({ api: currentApi, initialTasks: [current], pollingIntervalMs: 0, onError: vi.fn() }),
    { initialProps: { currentApi: firstApi } }
  );
  const refreshFromFirstApi = result.current.refreshTasks;
  let request: Promise<TaskSummary[]>;

  act(() => {
    request = refreshFromFirstApi();
  });
  rerender({ currentApi: secondApi });
  await act(async () => {
    oldResponse.resolve([stale]);
    await request!;
  });

  expect(result.current.tasks).toEqual([current]);
});

test("changing API generation resets tasks readiness and pending actions", async () => {
  const oldAction = deferred<TaskSummary[]>();
  const oldTask = summary({ state: "Running" });
  const firstApi = taskApi();
  firstApi.runAction = vi.fn(() => oldAction.promise);
  const secondApi = taskApi();
  const { result, rerender } = renderHook(
    ({ currentApi, initial }: { currentApi: TaskApi; initial?: TaskSummary[] }) =>
      useTasks({
        api: currentApi,
        initialTasks: initial,
        pollingIntervalMs: 0,
        onError: vi.fn()
      }),
    { initialProps: { currentApi: firstApi, initial: [oldTask] as TaskSummary[] | undefined } }
  );
  let oldRequest: Promise<void>;

  act(() => {
    oldRequest = result.current.onAction("pause", oldTask.id);
  });
  expect(result.current.pendingActions).toEqual({ [oldTask.id]: "pause" });

  rerender({ currentApi: secondApi, initial: undefined });
  await act(async () => undefined);
  expect(result.current.tasks).toEqual([]);
  expect(result.current.ready).toBe(false);
  expect(result.current.pendingActions).toEqual({});

  await act(async () => {
    oldAction.resolve([summary({ state: "Paused" })]);
    await oldRequest!;
  });
  expect(result.current.tasks).toEqual([]);
  expect(result.current.pendingActions).toEqual({});
});

test("changing API generation clears task removal tombstones", async () => {
  const cleared = summary({ state: "Completed" });
  const restored = summary({ state: "Running" });
  const firstApi = taskApi();
  firstApi.runAction = vi.fn(async () => []);
  const secondApi = taskApi();
  const { result, rerender } = renderHook(
    ({ currentApi, initial }: { currentApi: TaskApi; initial: TaskSummary[] }) =>
      useTasks({
        api: currentApi,
        initialTasks: initial,
        pollingIntervalMs: 0,
        onError: vi.fn()
      }),
    { initialProps: { currentApi: firstApi, initial: [cleared] } }
  );

  await act(async () => {
    await result.current.onAction("clear", cleared.id);
  });
  expect(result.current.tasks).toEqual([]);

  rerender({ currentApi: secondApi, initial: [restored] });
  await act(async () => undefined);

  expect(result.current.tasks).toEqual([restored]);
  expect(result.current.ready).toBe(true);
});

test("captured sync and upsert callbacks from an old API generation are ignored", async () => {
  const oldTask = summary({ id: "old-task", state: "Running" });
  const currentTask = summary({ id: "current-task", state: "Running" });
  const staleTask = summary({ id: "stale-task", state: "Completed" });
  const firstApi = taskApi();
  const secondApi = taskApi();
  const { result, rerender } = renderHook(
    ({ currentApi, initial }: { currentApi: TaskApi; initial: TaskSummary[] }) =>
      useTasks({
        api: currentApi,
        initialTasks: initial,
        pollingIntervalMs: 0,
        onError: vi.fn()
      }),
    { initialProps: { currentApi: firstApi, initial: [oldTask] } }
  );
  const oldSync = result.current.syncTasks;
  const oldUpsert = result.current.upsertTask;

  rerender({ currentApi: secondApi, initial: [currentTask] });
  await act(async () => undefined);
  act(() => {
    oldSync([staleTask]);
    oldUpsert(staleTask);
  });

  expect(result.current.tasks).toEqual([currentTask]);
});

test("StrictMode cleanup prevents settled polls from scheduling after unmount", async () => {
  vi.useFakeTimers();
  const polls: Array<ReturnType<typeof deferred<TaskSummary[]>>> = [];
  const api = taskApi();
  api.listTasks = vi.fn(() => {
    const poll = deferred<TaskSummary[]>();
    polls.push(poll);
    return poll.promise;
  });

  try {
    const { unmount } = renderHook(
      () => useTasks({ api, pollingIntervalMs: 1000, onError: vi.fn() }),
      { wrapper: StrictMode }
    );
    const callsAtUnmount = vi.mocked(api.listTasks).mock.calls.length;
    expect(callsAtUnmount).toBeGreaterThan(0);

    unmount();
    await act(async () => {
      for (const poll of polls) poll.resolve([]);
      await Promise.all(polls.map((poll) => poll.promise));
      await vi.advanceTimersByTimeAsync(5000);
    });

    expect(api.listTasks).toHaveBeenCalledTimes(callsAtUnmount);
  } finally {
    for (const poll of polls) poll.resolve([]);
    vi.clearAllTimers();
    vi.useRealTimers();
  }
});

test("a delayed subscription is disposed when it resolves after unmount", async () => {
  const subscription = deferred<() => void>();
  const unlisten = vi.fn();
  const api = taskApi();
  api.subscribe = vi.fn(() => subscription.promise);
  const { unmount } = renderHook(() =>
    useTasks({ api, pollingIntervalMs: 0, onError: vi.fn() })
  );

  unmount();
  await act(async () => {
    subscription.resolve(unlisten);
    await subscription.promise;
  });

  expect(unlisten).toHaveBeenCalledTimes(1);
});

test("polling waits for the current request before scheduling another one", async () => {
  vi.useFakeTimers();
  const poll = deferred<TaskSummary[]>();
  const api = taskApi();
  api.listTasks = vi.fn(() => poll.promise);
  let unmount: (() => void) | undefined;

  try {
    ({ unmount } = renderHook(() =>
      useTasks({ api, pollingIntervalMs: 1000, onError: vi.fn() })
    ));

    expect(api.listTasks).toHaveBeenCalledTimes(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });

    expect(api.listTasks).toHaveBeenCalledTimes(1);
  } finally {
    unmount?.();
    poll.resolve([]);
    await act(async () => {
      await poll.promise;
    });
    vi.clearAllTimers();
    vi.useRealTimers();
  }
});
