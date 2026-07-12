# Transfer Center Hardening Design

## Goal

Finish the in-progress transfer-center scaling work without adding later roadmap features. The deliverable fixes five confirmed correctness and privacy defects, validates a real Tauri window under development and production CSP configurations, and produces one local commit named `feat(tasks): scale the transfer center` without pushing it.

## Scope

This milestone includes:

1. Preventing a fast upload, delete, or catalog rebuild from completing before the frontend observes the catalog-invalidating task.
2. Preventing a delayed polling response from replacing a newer task event snapshot.
3. Refreshing expanded task-item pages when a task reaches a terminal state and preventing callback churn from starving active refresh timers.
4. Advertising retry only when a failed task has a valid persisted `TaskSpec`.
5. Removing local absolute paths and source metadata from values serialized to the WebView.
6. Running complete automated checks, a real Tauri development-window smoke test, and a production-CSP build-window smoke test.

The milestone excludes file decomposition, prompt/confirm replacement, drag-and-drop, main catalog virtualization, retention policies, staging migration, packaging enablement, documentation expansion, CI work, and the 2 GiB end-to-end recovery exercise.

## Confirmed root causes

### Task snapshot races

`useTasks` currently lets event callbacks and polling promises replace the whole task array unconditionally. Polling uses `setInterval`, so requests can overlap. A poll that began before a `Completed` event can resolve afterward and regress the UI to `Running`.

The generic `run` helper also starts a task refresh before the enqueue promise completes. Upload and delete discard the enqueue response. Catalog reload is inferred from an observed non-completed to completed transition, so a task first observed as completed does not invalidate the catalog.

### Stale expanded details

Task item pages are cached after expansion. Active tasks refresh visible pages every five seconds, but entering `Completed`, `Failed`, or `Canceled` stops the timer without a final refresh. The inline `onError` callback from `App` changes identity every render and repeatedly rebuilds the page loader and timer.

### Incorrect retry affordance

The backend only requeues failed tasks whose `spec_json` exists. The frontend derives actions only from `TaskState`, so every failed task displays Retry even when the backend must reject it.

### WebView path exposure

Enqueue commands return `TaskRecord`, whose items can contain `source_path` and `source_modified_at_ns`. Several local-file errors also embed absolute paths in `Unsupported`, IO, walkdir, or path-validation messages that become command errors and persisted task errors.

## Architecture

### Monotonic task snapshot coordinator

`useTasks` remains the owner of task summary state. It will maintain an accepted-snapshot revision and a single in-flight background pull:

- An event snapshot is accepted immediately and advances the revision.
- A poll or explicit list request captures the revision before starting. Its result is accepted only if no newer snapshot was accepted while it was in flight.
- Background polling schedules the next pull only after the previous pull settles, so polls cannot overlap.
- An enqueue summary is merged by task ID and `updated_at`; an older queued response cannot overwrite a newer terminal event.
- Task action responses use the same guarded acceptance path. If their corresponding event already arrived, the response is ignored as redundant.
- `current_setup` no longer repeatedly replaces task state; the task API event/poll path remains the authoritative ongoing source.

### Catalog completion ledger

The application will track IDs of catalog-mutating tasks whose completion has already been handled. The first authoritative task snapshot seeds the ledger without reloading old history. After that, a newly observed completed upload, delete, or rebuild triggers one catalog reload even when no intermediate task state was rendered. Completion IDs remain recorded so stale or repeated snapshots cannot reload twice.

The pre-enqueue task refresh is removed. All enqueue commands return a `TaskSummary`; the frontend safely merges that summary and then relies on events and guarded polling for progress.

### Detail cache invalidation

`TaskDetails` records the previous task state. A transition into `Completed`, `Failed`, or `Canceled` forces one refresh of every visible cached page before polling stops. Active five-second refresh remains limited to visible pages. `App` passes a memoized task-error callback so ordinary summary renders do not recreate the loader or reset the timer.

### Server-authoritative retry capability

`TaskSummary` gains `can_retry`. The read model sets it only when the task is failed and its persisted `spec_json` successfully deserializes as `TaskSpec`. Malformed specs do not make summary listing fail; they simply produce `can_retry = false`. The backend retry command retains its atomic database check because UI capability data can become stale before a click.

The frontend changes from `taskActionsForState` to task-aware action selection. A failed task with `can_retry = false` offers only Clear.

### WebView-safe DTO and errors

Tauri enqueue commands return `TaskSummary` instead of `TaskRecord`. Task-item page DTOs continue to expose only IDs, relative names, sizes, states, phases, progress, and safe errors. Frontend task types remove fields the backend deliberately omits.

Command-error conversion replaces known local-path-bearing errors with stable messages that describe the operation without the path. IO and directory traversal failures use generic local-storage messages. Remote object paths, node IDs, structured error codes, retryability, and safe status details remain available. The same safe command message is persisted as a task error, preventing later summary events from re-exposing an absolute path.

## Testing strategy

Every behavior change follows red-green-refactor:

- Deferred-promise hook tests reproduce an event followed by a stale poll and overlapping polls.
- Catalog completion tests cover first-snapshot baseline behavior and a newly seen completed mutation without an observed intermediate state.
- Component tests rerender an expanded running task as terminal and require an immediate item-page refresh. A timer test confirms summary rerenders do not starve active refresh.
- Rust storage tests cover retry capability for valid, missing, and malformed specs. Frontend presentation tests consume `can_retry`.
- Tauri serialization tests prove enqueue summaries and task item pages omit source paths and modification metadata.
- Command error tests inject sentinel Windows and Unix absolute paths and assert they do not appear in serialized or persisted WebView-facing messages.

## Acceptance criteria

The milestone is complete only when:

1. All five regression tests fail against the pre-fix implementation and pass after the fix.
2. Existing Rust and frontend suites remain green.
3. Strict Clippy, formatting, all-target checks, release compilation, production frontend build, diff checks, and token/path scans pass.
4. A real Tauri development window starts and the transfer center renders and responds.
5. A production-CSP build window starts without CSP console failures that block the application.
6. All spawned Vite, Tauri, and Lios processes are stopped.
7. The complete milestone is committed once as `feat(tasks): scale the transfer center` and is not pushed.
