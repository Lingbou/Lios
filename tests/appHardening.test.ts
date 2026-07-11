import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("task progress uses native progress elements without inline style props", async () => {
  const app = await readFile(new URL("src/App.tsx", root), "utf8");

  assert.doesNotMatch(app, /\bstyle\s*=/);
  assert.equal(app.match(/<progress\b/g)?.length, 2);
});

test("frontend verification includes the app hardening scan", async () => {
  const packageJson = JSON.parse(await readFile(new URL("package.json", root), "utf8")) as {
    scripts: Record<string, string>;
  };

  assert.match(packageJson.scripts["test:frontend"], /appHardening\.test\.ts/);
});
