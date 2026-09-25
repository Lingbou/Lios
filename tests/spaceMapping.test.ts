import assert from "node:assert/strict";
import test from "node:test";

import { sanitizeSpaceAlias } from "../src/features/spaces/spaceMapping.ts";

test("preserves valid lowercase space names", () => {
  assert.equal(sanitizeSpaceAlias("my-dataset"), "my-dataset");
  assert.equal(sanitizeSpaceAlias("valid_space_1"), "valid_space_1");
});

test("adds s_ prefix when dataset starts with a digit", () => {
  assert.equal(sanitizeSpaceAlias("123_data"), "s_123_data");
  assert.equal(sanitizeSpaceAlias("999"), "s_999");
});

test("replaces dots and unsupported symbols with underscore", () => {
  assert.equal(sanitizeSpaceAlias("v1.0-data"), "v1_0-data");
  assert.equal(sanitizeSpaceAlias("pkg.sub.model"), "pkg_sub_model");
});

test("converts uppercase to lowercase", () => {
  assert.equal(sanitizeSpaceAlias("MY_DATASET"), "my_dataset");
});

test("handles leading and trailing underscores cleanly", () => {
  assert.equal(sanitizeSpaceAlias("_secret_repo_"), "secret_repo");
});

test("truncates names exceeding 32 characters and trims trailing delimiters", () => {
  const longName = "abcdefghijklmnopqrstuvwxyz0123456789extra";
  const result = sanitizeSpaceAlias(longName);
  assert.ok(result.length <= 32);
  assert.ok(/^[a-z][a-z0-9_-]{0,31}$/.test(result));
});

test("resolves alias collisions with takenAliases set", () => {
  const taken = new Set(["s_123", "s_123_2"]);
  assert.equal(sanitizeSpaceAlias("123", taken), "s_123_3");
});
