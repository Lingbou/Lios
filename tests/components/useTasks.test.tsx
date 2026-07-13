import { act, renderHook } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import { useTasks } from "../../src/features/tasks/useTasks.ts";
import type {
  TaskApi,
  TaskSummary,
  TaskUpdateEvent
} from "../../src/features/tasks/taskTypes.ts";

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
    getTask: vi.fn(async () => null),
    listTaskItems: vi.fn(async (taskId, offset) => ({
      task_id: taskId,
      offset,
      total: 0,
      items: []
    })),
    runAction: vi.fn(async () => undefined),
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

async function flushEffects() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

test("an older or equal enqueue summary cannot replace a newer task state", () => {
  const running = summary({
    state: "Running",
    updated_at: "2026-07-12T00:00:02Z"
  });
  const { result } = renderHook(() =>
    useTasks({
      api: taskApi(),
      initialTasks: [running],
      fallbackAfterMs: 0,
      onError: vi.fn()
    })
  );

  act(() => {
    result.current.upsertTask(
      summary({ state: "Queued", updated_at: "2026-07-12T00:00:01Z" })
    );
    result.current.upsertTask(
      summary({ state: "Queued", updated_at: "2026-07-12T00:00:02Z" })
    );
  });

  expect(result.current.tasks).toEqual([running]);
});

test("a targeted event received during a list request cannot be overwritten by its stale response", async () => {
  const list = deferred<TaskSummary[]>();
  const running = summary({
    state: "Running",
    updated_at: "2026-07-12T00:00:01Z"
  });
  const completed = summary({
    state: "Completed",
    updated_at: "2026-07-12T00:00:02Z"
  });
  let listener: ((event: TaskUpdateEvent) => void) | undefined;
  const api = taskApi();
  api.listTasks = vi.fn(() => list.promise);
  api.subscribe = vi.fn(async (nextListener) => {
    listener = nextListener;
    return () => undefined;
  });

  const { result } = renderHook(() =>
    useTasks({ api, fallbackAfterMs: 0, onError: vi.fn() })
  );
  await flushEffects();
  act(() => listener?.({ kind: "upsert", task: completed }));
  expect(result.current.tasks).toEqual([completed]);

  await act(async () => {
    list.resolve([running]);
    await list.promise;
  });

  expect(result.current.tasks).toEqual([completed]);
});

test("terminal and paused tasks do not start fallback polling", async () => {
  vi.useFakeTimers();
  try {
    const completed = summary({ state: "Completed" });
    const paused = summary({ id: "task-b", state: "Paused" });
    const api = taskApi();
    api.listTasks = vi.fn(async () => [completed, paused]);
    renderHook(() =>
      useTasks({
        api,
        initialTasks: [completed, paused],
        fallbackAfterMs: 10_000,
        onError: vi.fn()
      })
    );
    await flushEffects();
    expect(api.listTasks).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });

    expect(api.listTasks).toHaveBeenCalledTimes(1);
  } finally {
    vi.useRealTimers();
  }
});

test("an active task falls back to one list query after ten seconds without events", async () => {
  vi.useFakeTimers();
  try {
    const running = summary({ state: "Running" });
    const api = taskApi();
    api.listTasks = vi.fn(async () => [running]);
    renderHook(() =>
      useTasks({
        api,
        initialTasks: [running],
        fallbackAfterMs: 10_000,
        onError: vi.fn()
      })
    );
    await flushEffects();
    expect(api.listTasks).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(9_999);
    });
    expect(api.listTasks).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(api.listTasks).toHaveBeenCalledTimes(2);
  } finally {
    vi.useRealTimers();
  }
});

test("a task event resets the ten second fallback window", async () => {
  vi.useFakeTimers();
  try {
    const running = summary({ state: "Running" });
    let listener: ((event: TaskUpdateEvent) => void) | undefined;
    const api = taskApi();
    api.listTasks = vi.fn(async () => [running]);
    api.subscribe = vi.fn(async (nextListener) => {
      listener = nextListener;
      return () => undefined;
    });
    renderHook(() =>
      useTasks({
        api,
        initialTasks: [running],
        fallbackAfterMs: 10_000,
        onError: vi.fn()
      })
    );
    await flushEffects();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(9_000);
    });
    act(() =>
      listener?.({
        kind: "upsert",
        task: summary({
          state: "Running",
          updated_at: "2026-07-12T00:00:01Z"
        })
      })
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(9_999);
    });
    expect(api.listTasks).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(api.listTasks).toHaveBeenCalledTimes(2);
  } finally {
    vi.useRealTimers();
  }
});

test("actions immediately refresh only the target task", async () => {
  const running = summary({ state: "Running" });
  const paused = summary({
    state: "Paused",
    updated_at: "2026-07-12T00:00:01Z"
  });
  const api = taskApi();
  api.listTasks = vi.fn(async () => [running]);
  api.getTask = vi.fn(async () => paused);
  const { result } = renderHook(() =>
    useTasks({
      api,
      initialTasks: [running],
      fallbackAfterMs: 0,
      onError: vi.fn()
    })
  );
  await flushEffects();

  await act(async () => {
    await result.current.onAction("pause", running.id);
  });

  expect(api.runAction).toHaveBeenCalledWith("pause", running.id);
  expect(api.getTask).toHaveBeenCalledWith(running.id);
  expect(result.current.tasks).toEqual([paused]);
  expect(api.listTasks).toHaveBeenCalledTimes(1);
});

test("clear removes the target and ignores a late stale upsert event", async () => {
  const completed = summary({ state: "Completed" });
  let listener: ((event: TaskUpdateEvent) => void) | undefined;
  const api = taskApi();
  api.listTasks = vi.fn(async () => [completed]);
  api.getTask = vi.fn(async () => null);
  api.subscribe = vi.fn(async (nextListener) => {
    listener = nextListener;
    return () => undefined;
  });
  const { result } = renderHook(() =>
    useTasks({
      api,
      initialTasks: [completed],
      fallbackAfterMs: 0,
      onError: vi.fn()
    })
  );
  await flushEffects();

  await act(async () => {
    await result.current.onAction("clear", completed.id);
  });
  expect(result.current.tasks).toEqual([]);

  act(() => listener?.({ kind: "upsert", task: completed }));
  expect(result.current.tasks).toEqual([]);
});

test("task subscriptions are removed on unmount", async () => {
  const unlisten = vi.fn();
  const api = taskApi();
  api.subscribe = vi.fn(async () => unlisten);
  const { unmount } = renderHook(() =>
    useTasks({ api, fallbackAfterMs: 0, onError: vi.fn() })
  );
  await flushEffects();

  unmount();

  expect(unlisten).toHaveBeenCalledTimes(1);
});
