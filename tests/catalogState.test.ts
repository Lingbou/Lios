import assert from "node:assert/strict";
import test from "node:test";

import { initializeWithExistingCatalog, loadCatalogState } from "../src/catalogState.ts";
import { commandError as parseCommandError } from "../src/commandError.ts";
import { setupWarningMessage } from "../src/setupWarning.ts";

const commandError = (code: string, message = code) => ({
  code,
  message,
  retryable: false,
  details: null
});

test("existing catalog resolves to ready", async () => {
  const catalog = { tree: "existing" };

  const result = await loadCatalogState(async () => catalog);

  assert.deepEqual(result, { status: "ready", catalog });
});

test("only NotInitialized resolves to missing", async () => {
  const missing = await loadCatalogState(async () => {
    throw commandError("NotInitialized");
  });
  const authentication = commandError("Authentication", "invalid token");

  assert.deepEqual(missing, { status: "missing" });
  await assert.rejects(
    () =>
      loadCatalogState(async () => {
        throw authentication;
      }),
    (error) => error === authentication
  );
});

test("AlreadyInitialized reloads exactly once", async () => {
  let reloads = 0;

  const result = await initializeWithExistingCatalog(
    async () => {
      throw commandError("AlreadyInitialized");
    },
    async () => {
      reloads += 1;
      return "reloaded";
    }
  );

  assert.equal(result, "reloaded");
  assert.equal(reloads, 1);
});

test("AlreadyInitialized preserves reload failure", async () => {
  const reloadError = commandError("Network", "reload failed");

  await assert.rejects(
    () =>
      initializeWithExistingCatalog(
        async () => {
          throw commandError("AlreadyInitialized");
        },
        async () => {
          throw reloadError;
        }
      ),
    (error) => error === reloadError
  );
});

test("CommandError parser rejects unknown codes and non-boolean retryable values", () => {
  assert.equal(
    parseCommandError({
      code: "ExecuteArbitraryThing",
      message: "unsafe",
      retryable: false,
      details: null
    }),
    null
  );
  assert.equal(
    parseCommandError({
      code: "Network",
      message: "network failed",
      retryable: "false",
      details: null
    }),
    null
  );
});

test("CommandError parser accepts the declared structured shape", () => {
  assert.deepEqual(
    parseCommandError({
      code: "Network",
      message: "network failed",
      retryable: true,
      details: { status: null }
    }),
    {
      code: "Network",
      message: "network failed",
      retryable: true,
      details: { status: null }
    }
  );
});

test("setup reconnect warning supplies non-modal notice text", () => {
  assert.equal(
    setupWarningMessage({
      code: "ReconnectRequired",
      message: "Reconnect the ModelScope space."
    }),
    "Reconnect the ModelScope space."
  );
  assert.equal(setupWarningMessage(null), null);
});
