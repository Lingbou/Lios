import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, test, vi } from "vitest";
import { useTasks } from "../../src/features/tasks/useTasks.ts";
import { TaskCenter } from "../../src/features/tasks/TaskCenter.tsx";
import type {
  TaskAction,
  TaskApi,
  TaskItem,
  TaskItemsPage,
  TaskSummary
} from "../../src/features/tasks/taskTypes.ts";

function summary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-a",
    account_id: "account",
    space_id: "space",
    state: "Running",
    label: "upload",
    phase: "uploading",
    progress_total: 200,
    progress_done: 5,
    bytes_total: 2000,
    bytes_done: 50,
    speed_bps: 10,
    eta_seconds: 195,
    attempt: 1,
    created_at: "2026-07-12T00:00:00Z",
    updated_at: "2026-07-12T00:00:01Z",
    error: null,
    item_count: 200,
    can_retry: false,
    ...overrides
  };
}

function item(index: number, taskId = "task-a"): TaskItem {
  return {
    id: `${taskId}-item-${index}`,
    task_id: taskId,
    name: `file-${index}.bin`,
    relative_path: `folder/file-${index}.bin`,
    size: 100,
    state: "Running",
    phase: "uploading",
    bytes_done: 25,
    bytes_total: 100,
    error: null
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

test("failed tasks expose retry only when the server summary allows it", () => {
  const props = {
    pendingActions: {},
    onAction: async () => undefined,
    listTaskItems: async (taskId: string, offset: number) => ({
      task_id: taskId,
      offset,
      total: 0,
      items: []
    })
  };
  const { rerender } = render(
    <TaskCenter tasks={[summary({ state: "Failed", can_retry: false })]} {...props} />
  );

  expect(screen.queryByRole("button", { name: "重试任务" })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "清除记录" })).toBeEnabled();

  rerender(<TaskCenter tasks={[summary({ state: "Failed", can_retry: true })]} {...props} />);

  expect(screen.getByRole("button", { name: "重试任务" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "清除记录" })).toBeEnabled();
});

test("task details load lazily only after expansion", async () => {
  const listTaskItems = vi.fn<
    (taskId: string, offset: number, limit: number) => Promise<TaskItemsPage>
  >(async (taskId, offset, limit) => ({
    task_id: taskId,
    offset,
    total: 200,
    items: Array.from({ length: limit }, (_, index) => item(offset + index, taskId))
  }));

  render(
    <TaskCenter
      tasks={[summary()]}
      pendingActions={{}}
      onAction={async () => undefined}
      listTaskItems={listTaskItems}
    />
  );

  expect(listTaskItems).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("button", { name: "展开任务详情" }));
  await waitFor(() => expect(listTaskItems).toHaveBeenCalledTimes(1));
  expect(listTaskItems.mock.calls[0]?.[0]).toBe("task-a");
  expect(listTaskItems.mock.calls[0]?.[1]).toBe(0);
  expect(listTaskItems.mock.calls[0]?.[2]).toBeLessThanOrEqual(100);
});

test("a pending action disables only that task controls", async () => {
  let resolveAction: ((tasks: TaskSummary[]) => void) | undefined;
  const actionPromise = new Promise<TaskSummary[]>((resolve) => {
    resolveAction = resolve;
  });
  const api: TaskApi = {
    listTasks: async () => [summary(), summary({ id: "task-b" })],
    listTaskItems: async (taskId, offset) => ({ task_id: taskId, offset, total: 0, items: [] }),
    runAction: async () => actionPromise,
    subscribe: async () => () => undefined
  };

  function Harness() {
    const tasks = useTasks({
      api,
      initialTasks: [summary(), summary({ id: "task-b" })],
      pollingIntervalMs: 0,
      onError: () => undefined
    });
    return <TaskCenter {...tasks} />;
  }

  render(<Harness />);
  const rows = screen.getAllByRole("article");
  await userEvent.click(within(rows[0]).getByRole("button", { name: "暂停任务" }));

  await waitFor(() =>
    expect(within(rows[0]).getByRole("button", { name: "暂停任务" })).toBeDisabled()
  );
  expect(within(rows[0]).getByRole("button", { name: "取消任务" })).toBeDisabled();
  expect(within(rows[1]).getByRole("button", { name: "暂停任务" })).toBeEnabled();
  expect(within(rows[1]).getByRole("button", { name: "取消任务" })).toBeEnabled();

  resolveAction?.([summary(), summary({ id: "task-b" })]);
  await waitFor(() =>
    expect(within(rows[0]).getByRole("button", { name: "暂停任务" })).toBeEnabled()
  );
});

test("virtual detail rows request only pages that become visible", async () => {
  const requests: Array<[string, number, number]> = [];
  const listTaskItems = async (taskId: string, offset: number, limit: number) => {
    requests.push([taskId, offset, limit]);
    return {
      task_id: taskId,
      offset,
      total: 250,
      items: Array.from({ length: limit }, (_, index) => item(offset + index, taskId))
    };
  };

  const { container } = render(
    <TaskCenter
      tasks={[summary({ item_count: 250 })]}
      pendingActions={{}}
      onAction={async (_action: TaskAction) => undefined}
      listTaskItems={listTaskItems}
    />
  );
  await userEvent.click(screen.getByRole("button", { name: "展开任务详情" }));
  await waitFor(() => expect(requests).toEqual([["task-a", 0, 50]]));

  const viewport = container.querySelector<HTMLElement>(".taskDetailViewport");
  expect(viewport).not.toBeNull();
  Object.defineProperty(viewport!, "clientHeight", { configurable: true, value: 240 });
  viewport!.scrollTop = 2600;
  fireEvent.scroll(viewport!);

  await waitFor(() => expect(requests.some(([, offset]) => offset >= 50)).toBe(true));
  expect(requests.every(([, , limit]) => limit <= 100)).toBe(true);
  expect(container.querySelectorAll(".taskVirtualRow").length).toBeLessThan(40);
});

test.each(["Completed", "Failed", "Canceled"] as const)(
  "refreshes every visible detail page once when a running task becomes %s",
  async (terminalState) => {
    const listTaskItems = vi.fn<
      (taskId: string, offset: number, limit: number) => Promise<TaskItemsPage>
    >(async (taskId, offset, limit) => ({
      task_id: taskId,
      offset,
      total: 200,
      items: Array.from({ length: limit }, (_, index) => item(offset + index, taskId))
    }));
    const props = {
      pendingActions: {},
      onAction: async () => undefined,
      listTaskItems
    };
    const { container, rerender } = render(
      <TaskCenter tasks={[summary()]} {...props} />
    );

    fireEvent.click(container.querySelector<HTMLButtonElement>(".taskExpandButton")!);
    await waitFor(() => expect(listTaskItems).toHaveBeenCalledWith("task-a", 0, 50));

    const viewport = container.querySelector<HTMLElement>(".taskDetailViewport")!;
    Object.defineProperty(viewport, "clientHeight", { configurable: true, value: 240 });
    viewport.scrollTop = 2600;
    fireEvent.scroll(viewport);
    await waitFor(() => expect(listTaskItems).toHaveBeenCalledWith("task-a", 50, 50));
    listTaskItems.mockClear();

    rerender(
      <TaskCenter
        tasks={[summary({ state: terminalState, updated_at: "2026-07-12T00:00:02Z" })]}
        {...props}
      />
    );

    await waitFor(() =>
      expect(listTaskItems.mock.calls.map(([, offset]) => offset).sort((left, right) => left - right))
        .toEqual([0, 50])
    );

    rerender(
      <TaskCenter
        tasks={[summary({ state: terminalState, updated_at: "2026-07-12T00:00:03Z" })]}
        {...props}
      />
    );
    await waitFor(() => expect(listTaskItems).toHaveBeenCalledTimes(2));
  }
);

test.each(["Canceled", "Failed"] as const)(
  "refreshes visible detail pages once when a paused task becomes %s",
  async (terminalState) => {
    const listTaskItems = vi.fn<
      (taskId: string, offset: number, limit: number) => Promise<TaskItemsPage>
    >(async (taskId, offset, limit) => ({
      task_id: taskId,
      offset,
      total: 200,
      items: Array.from({ length: limit }, (_, index) => item(offset + index, taskId))
    }));
    const props = {
      pendingActions: {},
      onAction: async () => undefined,
      listTaskItems
    };
    const { container, rerender } = render(
      <TaskCenter tasks={[summary({ state: "Paused" })]} {...props} />
    );

    fireEvent.click(container.querySelector<HTMLButtonElement>(".taskExpandButton")!);
    await waitFor(() =>
      expect(container.querySelector(".taskVirtualRow.running")).not.toBeNull()
    );
    listTaskItems.mockClear();

    rerender(
      <TaskCenter
        tasks={[summary({ state: terminalState, updated_at: "2026-07-12T00:00:02Z" })]}
        {...props}
      />
    );
    await waitFor(() => expect(listTaskItems).toHaveBeenCalledTimes(1));

    rerender(
      <TaskCenter
        tasks={[summary({ state: terminalState, updated_at: "2026-07-12T00:00:03Z" })]}
        {...props}
      />
    );
    await waitFor(() => expect(listTaskItems).toHaveBeenCalledTimes(1));
  }
);

test("queues a terminal refresh behind an in-flight active refresh", async () => {
  vi.useFakeTimers();
  const activeRefresh = deferred<TaskItemsPage>();
  const terminalRefresh = deferred<TaskItemsPage>();
  const page = (state: TaskItem["state"]): TaskItemsPage => ({
    task_id: "task-a",
    offset: 0,
    total: 1,
    items: [
      {
        ...item(0),
        state,
        bytes_done: state === "Completed" ? 100 : 25
      }
    ]
  });
  const listTaskItems = vi.fn<
    (taskId: string, offset: number, limit: number) => Promise<TaskItemsPage>
  >();
  listTaskItems
    .mockResolvedValueOnce(page("Running"))
    .mockImplementationOnce(() => activeRefresh.promise)
    .mockImplementationOnce(() => terminalRefresh.promise);
  const props = {
    pendingActions: {},
    onAction: async () => undefined,
    listTaskItems
  };
  let unmount: (() => void) | undefined;

  try {
    const rendered = render(
      <TaskCenter tasks={[summary({ item_count: 1 })]} {...props} />
    );
    unmount = rendered.unmount;
    fireEvent.click(rendered.container.querySelector<HTMLButtonElement>(".taskExpandButton")!);
    await act(async () => undefined);
    expect(rendered.container.querySelector(".taskVirtualRow.running")).not.toBeNull();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(listTaskItems).toHaveBeenCalledTimes(2);

    rendered.rerender(
      <TaskCenter
        tasks={[summary({ item_count: 1, state: "Completed", updated_at: "2026-07-12T00:00:02Z" })]}
        {...props}
      />
    );
    expect(listTaskItems).toHaveBeenCalledTimes(2);

    await act(async () => {
      activeRefresh.resolve(page("Running"));
      await activeRefresh.promise;
    });
    expect(listTaskItems).toHaveBeenCalledTimes(3);

    await act(async () => {
      terminalRefresh.resolve(page("Completed"));
      await terminalRefresh.promise;
    });
    expect(rendered.container.querySelector(".taskVirtualRow.completed")).not.toBeNull();
  } finally {
    unmount?.();
    vi.clearAllTimers();
    vi.useRealTimers();
  }
});

test.each(["collapse", "unmount"] as const)(
  "does not run a queued forced refresh after detail %s",
  async (cleanupMode) => {
    vi.useFakeTimers();
    const activeRefresh = deferred<TaskItemsPage>();
    const runningPage: TaskItemsPage = {
      task_id: "task-a",
      offset: 0,
      total: 1,
      items: [item(0)]
    };
    const completedPage: TaskItemsPage = {
      ...runningPage,
      items: [{ ...item(0), state: "Completed", bytes_done: 100 }]
    };
    const listTaskItems = vi.fn<
      (taskId: string, offset: number, limit: number) => Promise<TaskItemsPage>
    >();
    listTaskItems
      .mockResolvedValueOnce(runningPage)
      .mockImplementationOnce(() => activeRefresh.promise)
      .mockResolvedValue(completedPage);
    const props = {
      pendingActions: {},
      onAction: async () => undefined,
      listTaskItems
    };
    let unmount: (() => void) | undefined;

    try {
      const rendered = render(
        <TaskCenter tasks={[summary({ item_count: 1 })]} {...props} />
      );
      unmount = rendered.unmount;
      fireEvent.click(rendered.container.querySelector<HTMLButtonElement>(".taskExpandButton")!);
      await act(async () => undefined);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(5000);
      });
      expect(listTaskItems).toHaveBeenCalledTimes(2);

      rendered.rerender(
        <TaskCenter
          tasks={[summary({ item_count: 1, state: "Completed", updated_at: "2026-07-12T00:00:02Z" })]}
          {...props}
        />
      );
      if (cleanupMode === "collapse") {
        fireEvent.click(rendered.container.querySelector<HTMLButtonElement>(".taskExpandButton")!);
      } else {
        rendered.unmount();
        unmount = undefined;
      }

      await act(async () => {
        activeRefresh.resolve(runningPage);
        await activeRefresh.promise;
        await Promise.resolve();
      });
      expect(listTaskItems).toHaveBeenCalledTimes(2);
    } finally {
      unmount?.();
      vi.clearAllTimers();
      vi.useRealTimers();
    }
  }
);

test("active detail polling keeps its cadence across summary rerenders", async () => {
  vi.useFakeTimers();
  const listTaskItems = vi.fn<
    (taskId: string, offset: number, limit: number) => Promise<TaskItemsPage>
  >(async (taskId, offset, limit) => ({
    task_id: taskId,
    offset,
    total: 200,
    items: Array.from({ length: limit }, (_, index) => item(offset + index, taskId))
  }));
  const props = {
    pendingActions: {},
    onAction: async () => undefined,
    listTaskItems
  };
  let unmount: (() => void) | undefined;

  try {
    const rendered = render(
      <TaskCenter tasks={[summary()]} {...props} onError={() => undefined} />
    );
    unmount = rendered.unmount;
    fireEvent.click(rendered.container.querySelector<HTMLButtonElement>(".taskExpandButton")!);
    expect(listTaskItems).toHaveBeenCalledTimes(1);
    await act(async () => undefined);
    listTaskItems.mockClear();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    rendered.rerender(
      <TaskCenter
        tasks={[summary({ progress_done: 10, updated_at: "2026-07-12T00:00:02Z" })]}
        {...props}
        onError={() => undefined}
      />
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    rendered.rerender(
      <TaskCenter
        tasks={[summary({ progress_done: 15, updated_at: "2026-07-12T00:00:03Z" })]}
        {...props}
        onError={() => undefined}
      />
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(listTaskItems).toHaveBeenCalledTimes(1);
    expect(listTaskItems).toHaveBeenLastCalledWith("task-a", 0, 50);
  } finally {
    unmount?.();
    vi.clearAllTimers();
    vi.useRealTimers();
  }
});
