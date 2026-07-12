# Transfer Center Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the five confirmed transfer-center correctness and privacy defects, validate real Tauri development and production-CSP windows, and create one local no-push commit.

**Architecture:** Keep task state ownership in the frontend task feature, but add monotonic snapshot arbitration so asynchronous sources cannot regress state. Make retry capability and WebView DTO safety server-authoritative, invalidate detail caches on terminal transitions, and infer catalog invalidation from a completion ledger rather than a fragile rendered-state edge.

**Tech Stack:** Rust 2021, rusqlite, Tauri 2, React 19, TypeScript, Vitest, Testing Library, Node test runner, PowerShell on Windows.

---

## File responsibility map

- `crates/lios-core/src/tasks.rs`: persisted task read model, `TaskSummary`, retry capability, item paging.
- `crates/lios-core/tests/local_state.rs`: SQLite task-summary and retry-capability regression tests.
- `src-tauri/src/lib.rs`: WebView DTOs, enqueue/list/action commands, task event serialization.
- `src-tauri/src/command_error.rs`: stable WebView-safe command error conversion.
- `src/features/tasks/taskTypes.ts`: frontend DTO contract.
- `src/features/tasks/taskApi.ts`: Tauri task command/event adapter.
- `src/features/tasks/useTasks.ts`: monotonic task snapshot coordinator and polling fallback.
- `src/features/tasks/taskPresentation.ts`: task-aware action and catalog-mutation presentation rules.
- `src/features/tasks/TaskCenter.tsx`: task row UI and detail-page cache lifecycle.
- `src/App.tsx`: catalog completion ledger, enqueue integration, stable error callback.
- `tests/components/useTasks.test.tsx`: asynchronous source-ordering tests.
- `tests/components/TaskCenter.test.tsx`: detail refresh and action rendering tests.
- `tests/taskPresentation.test.ts`: pure catalog completion and action capability tests.
- `docs/superpowers/specs/2026-07-12-transfer-center-hardening-design.md`: approved milestone design.

## Task 1: Make retry capability server-authoritative

**Files:**
- Modify: `crates/lios-core/src/tasks.rs:282-321, 1466-1573`
- Modify: `crates/lios-core/tests/local_state.rs:416-720`
- Modify: `src-tauri/src/lib.rs:4580-4620`
- Modify: `src/features/tasks/taskTypes.ts:22-42`
- Modify: `src/features/tasks/taskPresentation.ts:15-28`
- Modify: `src/features/tasks/TaskCenter.tsx:65-92`
- Modify: `tests/taskPresentation.test.ts:10-28`

- [ ] **Step 1: Write failing Rust summary capability tests**

Add a test that creates three failed tasks: one with a valid persisted `TaskSpec`, one inserted without a spec, and one whose `spec_json` is corrupted through a direct rusqlite update. Assert the summaries remain listable and expose `can_retry` as `true`, `false`, and `false` respectively.

```rust
#[test]
fn task_summaries_report_retry_capability_only_for_valid_specs() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let store = TaskStore::open(&db_path).unwrap();
    let spec = delete_spec();

    let valid = TaskRecord::queued_for_spec(&spec);
    store.insert_with_spec(&valid, &spec).unwrap();
    store.update_state(valid.id, TaskState::Failed, Some("failed".into())).unwrap();

    let missing = TaskRecord::queued("legacy", 0);
    store.insert(&missing).unwrap();
    store.update_state(missing.id, TaskState::Failed, Some("failed".into())).unwrap();

    let malformed = TaskRecord::queued_for_spec(&spec);
    store.insert_with_spec(&malformed, &spec).unwrap();
    store.update_state(malformed.id, TaskState::Failed, Some("failed".into())).unwrap();
    rusqlite::Connection::open(&db_path).unwrap()
        .execute(
            "UPDATE tasks SET spec_json = '{' WHERE id = ?1",
            rusqlite::params![malformed.id.to_string()],
        )
        .unwrap();

    let summaries = store.list_summaries().unwrap();
    assert!(summaries.iter().find(|task| task.id == valid.id).unwrap().can_retry);
    assert!(!summaries.iter().find(|task| task.id == missing.id).unwrap().can_retry);
    assert!(!summaries.iter().find(|task| task.id == malformed.id).unwrap().can_retry);
}
```

- [ ] **Step 2: Run the focused Rust test and verify RED**

Run:

```powershell
cargo test -p lios-core --test local_state task_summaries_report_retry_capability_only_for_valid_specs
```

Expected: compilation fails because `TaskSummary` has no `can_retry` field.

- [ ] **Step 3: Add `can_retry` to the Rust read model**

Extend the summary query tuple with `spec_json`. Decode it without propagating malformed-spec errors:

```rust
pub struct TaskSummary {
    // existing fields
    pub item_count: u64,
    pub can_retry: bool,
}

let parsed_state = TaskState::from_str(&state)?;
let can_retry = parsed_state == TaskState::Failed
    && spec_json
        .as_deref()
        .is_some_and(|json| serde_json::from_str::<TaskSpec>(json).is_ok());
```

Select `spec_json` in `list_summaries`, `get_summary`, and the queued/startup summary helper, and update every test constructor for `TaskSummary`.

- [ ] **Step 4: Verify the focused Rust test is GREEN**

Run the same command and expect one passing test.

- [ ] **Step 5: Write the failing frontend action test**

Replace the state-only assertion with task-aware input:

```typescript
assert.deepEqual(taskActionsForTask({ state: "Failed", can_retry: false }), ["clear"]);
assert.deepEqual(taskActionsForTask({ state: "Failed", can_retry: true }), ["retry", "clear"]);
```

- [ ] **Step 6: Run the frontend presentation test and verify RED**

```powershell
node --test tests/taskPresentation.test.ts
```

Expected: failure because `taskActionsForTask` does not exist.

- [ ] **Step 7: Implement task-aware action selection**

Add `can_retry` to `TaskSummary`, replace `taskActionsForState` with:

```typescript
export function taskActionsForTask(task: Pick<TaskSummary, "state" | "can_retry">): TaskAction[] {
  if (["Queued", "Preparing", "Running", "Retrying"].includes(task.state)) {
    return ["pause", "cancel"];
  }
  if (task.state === "Paused") return ["resume", "cancel"];
  if (task.state === "Failed") return task.can_retry ? ["retry", "clear"] : ["clear"];
  if (task.state === "Completed" || task.state === "Canceled") return ["clear"];
  return [];
}
```

Update `TaskActionButtons` to pass the complete task.

- [ ] **Step 8: Verify frontend presentation and component tests are GREEN**

```powershell
node --test tests/taskPresentation.test.ts
npx vitest run tests/components/TaskCenter.test.tsx
```

Expected: all focused tests pass.

## Task 2: Return WebView-safe task summaries from enqueue commands

**Files:**
- Modify: `src-tauri/src/lib.rs:136-173, 1143-1153, 4116-4235, 4327-4351`
- Modify: `src/features/tasks/taskTypes.ts:22-82`
- Modify: `src/App.tsx:31-40, 675-745, 919-962`
- Modify: `src/features/tasks/useTasks.ts:20-24`
- Test: `src-tauri/src/lib.rs:4528-4750`

- [ ] **Step 1: Write a failing serialization test for submission responses**

Create a task with a source item containing sentinel absolute paths. Convert the persisted submission response through the command helper and assert the serialized value contains summary fields, `item_count`, and `can_retry`, but not `items`, `source_path`, `source_modified_at_ns`, or the sentinel path.

```rust
#[test]
fn task_submission_response_is_a_safe_summary() {
    let summary = submission_summary_for_paths(&paths, task.id).unwrap();
    let json = serde_json::to_value(summary).unwrap();
    assert_eq!(json["item_count"], json!(1));
    assert_eq!(json["can_retry"], json!(false));
    assert!(json.get("items").is_none());
    assert!(!json.to_string().contains("C:\\\\private\\\\secret.bin"));
}
```

- [ ] **Step 2: Run the focused desktop test and verify RED**

```powershell
cargo test -p lios-desktop task_submission_response_is_a_safe_summary
```

Expected: failure because the helper and safe return type do not exist.

- [ ] **Step 3: Change submission and enqueue return types**

After `persist_submission`, load the summary from the store before emitting and spawning:

```rust
fn submit_and_spawn(
    app: &tauri::AppHandle,
    state: &AppContext,
    spec: TaskSpec,
    source_files: &[SourceFileSnapshot],
) -> CommandResult<TaskSummary> {
    let task = persist_submission(&state.paths, &spec, source_files).map_err(to_err)?;
    let summary = task_store(&state.paths)?
        .get_summary(task.id)
        .map_err(to_err)?
        .ok_or_else(|| CommandError::new(
            CommandErrorCode::CorruptedData,
            "persisted task summary is missing",
            false,
            None,
        ))?;
    emit_tasks(app, &state.paths);
    spawn_persisted_task(app.clone(), task.id);
    Ok(summary)
}
```

Change all `enqueue_*` command signatures to `CommandResult<TaskSummary>`.

- [ ] **Step 4: Remove sensitive frontend-only fields and `TaskRecord`**

Delete `source_path`, `source_modified_at_ns`, `TaskRecord`, and `taskRecordToSummary` from the WebView contract. Type every enqueue call as `TaskSummary` and merge the returned summary with `upsertTask`.

- [ ] **Step 5: Verify focused Rust and TypeScript checks are GREEN**

```powershell
cargo test -p lios-desktop task_submission_response_is_a_safe_summary
npx tsc --noEmit
```

Expected: both commands succeed.

## Task 3: Prevent stale and overlapping task polling

**Files:**
- Create: `tests/components/useTasks.test.tsx`
- Modify: `src/features/tasks/useTasks.ts:1-94`
- Modify: `tests/vitest.setup.ts`

- [ ] **Step 1: Write a deferred-promise stale-poll regression test**

Use a real hook harness. Start a polling `listTasks` promise that will return `Running`, deliver a `Completed` event, then release the poll and require the UI to remain completed.

```typescript
test("an older poll cannot replace a newer task event", async () => {
  const poll = deferred<TaskSummary[]>();
  let publish: ((tasks: TaskSummary[]) => void) | undefined;
  const api: TaskApi = {
    listTasks: vi.fn(() => poll.promise),
    listTaskItems: vi.fn(),
    runAction: vi.fn(),
    subscribe: async (listener) => { publish = listener; return () => undefined; }
  };

  render(<TaskStateHarness api={api} />);
  await waitFor(() => expect(api.listTasks).toHaveBeenCalledTimes(1));
  act(() => publish?.([summary({ state: "Completed", updated_at: "2026-07-12T00:00:02Z" })]));
  expect(screen.getByTestId("task-state")).toHaveTextContent("Completed");
  poll.resolve([summary({ state: "Running", updated_at: "2026-07-12T00:00:01Z" })]);
  await act(async () => undefined);
  expect(screen.getByTestId("task-state")).toHaveTextContent("Completed");
});
```

- [ ] **Step 2: Run the hook test and verify RED**

```powershell
npx vitest run tests/components/useTasks.test.tsx
```

Expected: final state is `Running`.

- [ ] **Step 3: Write the overlapping-poll regression test**

Use fake timers, leave the first `listTasks` unresolved, advance more than two polling intervals, and assert it was called only once.

- [ ] **Step 4: Verify the second test is RED**

Expected: current `setInterval` starts multiple calls.

- [ ] **Step 5: Implement guarded snapshot acceptance**

Add accepted-revision and request-sequence refs. Use one acceptance function for events, list results, task actions, setup seeding, and summary upserts. An upsert advances the revision only when it inserts a task or has an equal/newer `updated_at` than the stored task.

```typescript
const acceptedRevision = useRef(0);
const latestListRequest = useRef(0);

const acceptSnapshot = useCallback((nextTasks: TaskSummary[]) => {
  acceptedRevision.current += 1;
  setTasks(nextTasks);
}, []);

const requestTasks = useCallback(async () => {
  const revision = acceptedRevision.current;
  const request = ++latestListRequest.current;
  const nextTasks = await api.listTasks();
  if (revision === acceptedRevision.current && request === latestListRequest.current) {
    acceptSnapshot(nextTasks);
  }
  return nextTasks;
}, [acceptSnapshot, api]);
```

Replace `setInterval` with a recursive timeout scheduled in `finally` after each pull settles. Event delivery calls `acceptSnapshot` immediately. Guard action responses using the revision captured before `runAction`.

- [ ] **Step 6: Verify both hook regressions are GREEN**

Run the focused Vitest file and expect both tests to pass without pending-timer warnings.

## Task 4: Reload the catalog for newly observed completed mutations

**Files:**
- Modify: `src/features/tasks/taskPresentation.ts:30-55`
- Modify: `tests/taskPresentation.test.ts:150-180`
- Modify: `src/features/tasks/useTasks.ts`
- Modify: `src/App.tsx:372-536, 675-745, 919-962`

- [ ] **Step 1: Write failing completion-ledger tests**

Add a pure helper that accepts the handled ID set and a snapshot. Tests must establish an initial baseline without reload, then present a brand-new upload directly as completed and expect its ID exactly once.

```typescript
const handled = new Set<string>();
seedCatalogMutationCompletions(handled, [summary({ id: "old", state: "Completed" })]);
assert.deepEqual(newCatalogMutationCompletions(handled, [
  summary({ id: "old", state: "Completed" }),
  summary({ id: "fast", state: "Completed", label: "upload" })
]), ["fast"]);
assert.deepEqual(newCatalogMutationCompletions(handled, [
  summary({ id: "fast", state: "Completed", label: "upload" })
]), []);
```

- [ ] **Step 2: Run the presentation test and verify RED**

Expected: helper functions are missing.

- [ ] **Step 3: Implement completion-ledger helpers**

Recognize only `upload`, `delete`/`delete ...`, and `rebuild`. Seed the first authoritative snapshot, retain handled IDs after records disappear, and return only previously unhandled completed IDs.

- [ ] **Step 4: Integrate the ledger into `App`**

Return a `ready` flag from `useTasks`. When it first becomes ready, seed the ledger. On later snapshots, reload the active catalog once when the helper returns any ID. Remove `previousTaskStates` and the enqueue-before-completion edge dependency.

Remove the pre-action refresh from `run`:

```typescript
const pending = action();
await pending;
await refreshSetup(false);
await refreshTasks().catch(() => undefined);
```

Do not repeatedly copy `current_setup.tasks` into hook state. Merge each enqueue `TaskSummary` through the monotonic `upsertTask` path.

- [ ] **Step 5: Verify presentation, hook, and app build checks are GREEN**

```powershell
node --test tests/taskPresentation.test.ts
npx vitest run tests/components/useTasks.test.tsx
npx tsc --noEmit
```

Expected: all pass.

## Task 5: Refresh visible item pages at terminal transition

**Files:**
- Modify: `tests/components/TaskCenter.test.tsx`
- Modify: `src/features/tasks/TaskCenter.tsx:90-184`
- Modify: `src/App.tsx:1-40, 1486-1496`

- [ ] **Step 1: Write the failing terminal-refresh component test**

Make the first page call return running items and the second call completed items. Expand a running task, rerender the same ID as completed, and require an immediate second call and completed status text.

```typescript
const view = render(<TaskCenter tasks={[summary({ state: "Running" })]} {...props} />);
await userEvent.click(screen.getByRole("button", { name: "展开任务详情" }));
await waitFor(() => expect(listTaskItems).toHaveBeenCalledTimes(1));
view.rerender(<TaskCenter tasks={[summary({ state: "Completed" })]} {...props} />);
await waitFor(() => expect(listTaskItems).toHaveBeenCalledTimes(2));
expect(screen.getByText("已完成")).toBeInTheDocument();
```

- [ ] **Step 2: Run the component test and verify RED**

Expected: only one item-page call is made.

- [ ] **Step 3: Implement terminal cache invalidation**

Track the previous state in a ref. When the state changes from non-terminal to `Completed`, `Failed`, or `Canceled`, force-refresh all visible page indices exactly once.

```typescript
const previousState = useRef(task.state);
useEffect(() => {
  const enteredTerminal = !terminalStates.has(previousState.current)
    && terminalStates.has(task.state);
  previousState.current = task.state;
  if (enteredTerminal) {
    for (const pageIndex of visiblePages.current) void loadPage(pageIndex, true);
  }
}, [loadPage, task.state]);
```

Memoize the App error callback with `useCallback` and pass the same function to `useTasks` and `TaskCenter`.

- [ ] **Step 4: Add and verify the active-refresh stability test**

Rerender an active task several times with updated summary progress, advance fake timers beyond five seconds, and assert visible details refresh. Expect the test to pass only after callback identity and timer dependencies are stable.

- [ ] **Step 5: Verify the full TaskCenter component file is GREEN**

```powershell
npx vitest run tests/components/TaskCenter.test.tsx
```

## Task 6: Remove absolute local paths from WebView-facing errors

**Files:**
- Modify: `src-tauri/src/command_error.rs:85-168, 170-326`
- Modify: `src-tauri/src/lib.rs` task error persistence tests

- [ ] **Step 1: Write failing command-error path tests**

Create table-driven `LiosError` cases with sentinel Windows and Unix paths:

```rust
let sentinels = [r"C:\Users\Alice\private\secret.bin", "/home/alice/private/secret.bin"];
for path in sentinels {
    let cases = [
        LiosError::Unsupported(format!("source path no longer exists: {path}")),
        LiosError::Unsupported(format!("destination already exists: {path}")),
        LiosError::InvalidRelativePath(path.into()),
    ];
    for error in cases {
        let command = CommandError::from(error);
        assert!(!serde_json::to_string(&command).unwrap().contains(path));
    }
}
```

Add separate IO and walkdir cases and assert their messages are stable generic local-storage text.

- [ ] **Step 2: Run the command-error tests and verify RED**

```powershell
cargo test -p lios-desktop command_error::tests
```

Expected: sentinel paths appear in current serialized messages.

- [ ] **Step 3: Implement stable safe messages**

Map local path variants to operation-level messages:

```rust
fn safe_unsupported_message(message: String) -> String {
    const LOCAL_PREFIXES: &[(&str, &str)] = &[
        ("source path no longer exists:", "selected source no longer exists"),
        ("source file no longer exists:", "selected source file no longer exists"),
        ("source file changed while it was being packed:", "selected source file changed while it was being prepared"),
        ("source path changed before packing:", "selected source changed before it was prepared"),
        ("source path is not a file or directory:", "selected source is not a file or directory"),
        ("destination already exists:", "destination already exists"),
        ("upload source contains unsupported symbolic links or junctions:", "upload source contains unsupported symbolic links or junctions"),
        ("skipped ", "some selected upload paths are unsupported"),
    ];
    LOCAL_PREFIXES
        .iter()
        .find_map(|(prefix, safe)| message.starts_with(prefix).then(|| (*safe).to_string()))
        .unwrap_or(message)
}
```

Use generic messages for `MissingFileName`, `InvalidRelativePath`, IO, walkdir, and strip-prefix errors. Preserve typed codes, retryability, and safe remote status details.

- [ ] **Step 4: Add a persisted task-error regression**

Persist the safe `CommandError.message` into a failed task, serialize its `TaskSummary`, and assert the sentinel path is absent. This proves later task events cannot re-expose it.

- [ ] **Step 5: Verify all privacy tests are GREEN**

Run focused command-error and task-center backend tests.

## Task 7: Full automated verification and review

**Files:**
- Review: all modified and untracked milestone files

- [ ] **Step 1: Run focused suites together**

```powershell
cargo test -p lios-core --test local_state
cargo test -p lios-desktop
npm run test:frontend
```

Expected: zero failures; only the credentialed ModelScope live test may remain ignored.

- [ ] **Step 2: Run strict Rust and frontend verification**

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check --workspace --all-targets
cargo check --workspace --all-targets --release
cargo test --workspace
npm run build
git diff --check
```

Expected: all exit with code 0.

- [ ] **Step 3: Run path and token scans**

Scan tracked source output and tests for ModelScope token shapes, Authorization/Cookie values, sentinel local paths, and newly serialized `source_path` fields. Expected: no WebView-facing leak; legitimate persistence-only source fields remain confined to Rust task storage.

- [ ] **Step 4: Request spec compliance and code quality review**

Review every acceptance criterion in the approved design, then review the combined diff for race correctness, error handling, test realism, and unrelated changes. Resolve every important finding before smoke testing.

## Task 8: Real Tauri and production-CSP smoke

**Files:**
- Inspect: `src-tauri/tauri.conf.json`
- Inspect: `src-tauri/build_support.rs`
- Inspect: generated binaries under `target/`

- [ ] **Step 1: Confirm supported CLI build commands**

```powershell
npx tauri build --help
```

Choose the supported debug/no-bundle production build invocation shown by the installed CLI.

- [ ] **Step 2: Start and inspect the development Tauri window**

Run `npm run tauri dev`, wait for the real window, verify the application shell and transfer center render, exercise expand/collapse and available task buttons using existing local task history, and inspect terminal/browser output for runtime errors.

- [ ] **Step 3: Stop every development process**

Stop the Tauri process tree, Vite server, and Lios binary. Verify no listener remains on the development port and no `lios`, `cargo-tauri`, or Vite process remains.

- [ ] **Step 4: Build and inspect a production-CSP window**

Run the supported debug/no-bundle Tauri build, launch the produced executable, verify the shell and transfer center render under production CSP, and inspect output/logs for CSP-blocking failures.

- [ ] **Step 5: Stop every production smoke process**

Terminate the binary and verify no related listener or process remains.

## Task 9: Final verification and single local commit

**Files:**
- Stage: all intended source, test, lockfile, design, and plan files
- Exclude: `.codex/`, `dist/`, `node_modules/`, `target/`, logs, credentials, and local state

- [ ] **Step 1: Re-run the completion gate after smoke testing**

Run `git status`, `git diff --check`, focused tests, full Rust tests, frontend tests, and production frontend build again. Read each exit code and failure count.

- [ ] **Step 2: Inspect final scope**

Confirm the diff contains only the transfer-center milestone plus its approved design and plan. Pay special attention to all eight formerly untracked feature/test files so none are omitted.

- [ ] **Step 3: Stage the intended files**

Use explicit paths or `git add -A` followed by `git diff --cached --name-status`. Confirm ignored process artifacts and generated outputs are absent.

- [ ] **Step 4: Create the requested commit**

```powershell
git commit -m "feat(tasks): scale the transfer center"
```

- [ ] **Step 5: Verify the final repository state**

Confirm the commit exists, the working tree is clean, `main` is ahead of `origin/main`, and no push occurred.
