import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("task UI uses native progress elements without inline style props", async () => {
  const app = await readFile(new URL("src/App.tsx", root), "utf8");
  const taskCenter = await readFile(new URL("src/features/tasks/TaskCenter.tsx", root), "utf8");
  const taskSources = `${app}\n${taskCenter}`;

  assert.doesNotMatch(taskSources, /\bstyle\s*=/);
  assert.equal(taskCenter.match(/<progress\b/g)?.length, 2);
});

test("frontend verification includes the app hardening scan", async () => {
  const packageJson = JSON.parse(await readFile(new URL("package.json", root), "utf8")) as {
    scripts: Record<string, string>;
  };

  assert.match(packageJson.scripts["test:frontend"], /appHardening\.test\.ts/);
});

test("recovery catalog refresh preserves the active space task scope", async () => {
  const app = await readFile(new URL("src/App.tsx", root), "utf8");
  const body = app.match(
    /async function refreshVerifiedCatalog[\s\S]*?\n  async function confirmRecoveryKeyImport/
  )?.[0];

  assert.ok(body);
  assert.doesNotMatch(body, /setActiveSpace\(space\)/);
});

test("created space loading uses the scoped setup result", async () => {
  const app = await readFile(new URL("src/App.tsx", root), "utf8");
  const body = app.match(/async function submitCreateSpace[\s\S]*?\n  return \(/)?.[0];

  assert.ok(body);
  const setupResult = body.match(
    /const\s+([A-Za-z_$][\w$]*)\s*=\s*await refreshSetup\(true\);/
  );
  assert.ok(setupResult);
  assert.match(body, new RegExp(`await loadSpace\\(${setupResult[1]}\\)`));
  assert.doesNotMatch(body, /await loadSpace\(space\)/);
});

test("setup refresh returns the backend-scoped configured space despite a stale repo list", async () => {
  const app = await readFile(new URL("src/App.tsx", root), "utf8");
  const body = app.match(/async function refreshSetup[\s\S]*?\n  async function run/)?.[0];

  assert.ok(body);
  const scopedResult = body.match(
    /const\s+([A-Za-z_$][\w$]*)\s*:\s*SpaceSummary\s*\|\s*null\s*=\s*configuredRepo[\s\S]*?task_space_id:\s*next\.active_task_space_id\s*\?\?\s*undefined[\s\S]*?:\s*null;/
  );
  assert.ok(scopedResult);
  assert.match(body, new RegExp(`return ${scopedResult[1]};`));
});

test("catalog mutation baseline is seeded from the initial task query before completions are handled", async () => {
  const app = await readFile(new URL("src/App.tsx", root), "utf8");
  const completionEffect = app.match(
    /useEffect\(\(\) => \{[\s\S]*?seedCatalogMutationCompletions\(catalogMutationCompletions\.current, tasks\)[\s\S]*?newCatalogMutationCompletions[\s\S]*?\}, \[tasks, tasksReady, activeSpace\?\.task_space_id\]\);/
  )?.[0];

  assert.ok(completionEffect);
  const readyIndex = completionEffect.indexOf("if (!tasksReady) return");
  const seedIndex = completionEffect.indexOf(
    "seedCatalogMutationCompletions(catalogMutationCompletions.current, tasks)"
  );
  const completionIndex = completionEffect.indexOf("newCatalogMutationCompletions");
  assert.ok(readyIndex >= 0);
  assert.ok(seedIndex > readyIndex);
  assert.ok(completionIndex > seedIndex);
  assert.equal(app.match(/seedCatalogMutationCompletions\(/g)?.length, 1);
});

test("space catalog loads share one latest-only serial executor", async () => {
  const app = await readFile(new URL("src/App.tsx", root), "utf8");
  const loadSpace = app.match(/async function loadSpace[\s\S]*?\n  async function initializeActiveSpace/)?.[0];
  const reloadCatalog = app.match(/async function reloadCatalog[\s\S]*?\n  function toggleSelection/)?.[0];
  const refreshVerifiedCatalog = app.match(
    /async function refreshVerifiedCatalog[\s\S]*?\n  async function confirmRecoveryKeyImport/
  )?.[0];

  assert.match(app, /createLatestSerialExecutor/);
  assert.ok(loadSpace);
  assert.ok(reloadCatalog);
  assert.ok(refreshVerifiedCatalog);
  assert.match(loadSpace, /catalogLoads\.run/);
  assert.match(reloadCatalog, /catalogLoads\.run/);
  assert.match(refreshVerifiedCatalog, /catalogLoads\.run/);
  assert.match(app, /reloadCatalog\(false, activeSpace\)/);
});
