import assert from "node:assert/strict";
import test from "node:test";

import { parsePrTitle, validatePrTitle } from "./check-pr-title.mjs";

for (const title of [
  "feat: add document search",
  "fix(core): prevent duplicate turns",
  "feat(core)!: replace the storage format",
  "deps(cargo): update database dependencies",
  "revert: restore the previous storage behavior",
  "ci(deps): update the checkout action",
  "chore(main): release 0.1.0",
]) {
  test(`accepts ${title}`, () => {
    assert.equal(validatePrTitle(title), null);
  });
}

test("accepts a matching breaking-change footer", () => {
  assert.equal(
    validatePrTitle(
      "feat(core)!: replace the public API",
      "Migration details.\n\nBREAKING CHANGE: callers must use the new API.",
    ),
    null,
  );
});

test("rejects a hidden breaking-change footer", () => {
  assert.notEqual(
    validatePrTitle(
      "feat(core): replace the public API",
      "BREAKING CHANGE: callers must use the new API.",
    ),
    null,
  );
});

test("parses the release-relevant parts of a title", () => {
  assert.deepEqual(parsePrTitle("feat(core)!: replace the API"), {
    type: "feat",
    scope: "core",
    breaking: true,
    description: "replace the API",
  });
});

for (const title of [
  "Add document search",
  "feature: add document search",
  "feat(Core): add document search",
  "feat: ",
  "feat(core) add document search",
  "refactor(core)!: replace the public API",
  "chore!: reset the persisted format",
]) {
  test(`rejects ${title}`, () => {
    assert.notEqual(validatePrTitle(title), null);
  });
}
