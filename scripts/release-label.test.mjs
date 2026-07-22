import assert from "node:assert/strict";
import test from "node:test";

import { releaseLabel } from "./release-label.mjs";

for (const [title, label] of [
  ["feat: add search", "semver:minor"],
  ["fix(core): prevent a race", "semver:patch"],
  ["perf: reduce startup time", "semver:patch"],
  ["deps(cargo): update sqlite", "semver:patch"],
  ["revert: restore startup behavior", "semver:patch"],
  ["feat(core)!: replace the API", "semver:breaking"],
  ["docs: explain search", "semver:none"],
  ["ci: update checkout", "semver:none"],
]) {
  test(`maps ${title} to ${label}`, () => {
    assert.equal(releaseLabel(title), label);
  });
}

test("rejects an invalid title instead of guessing its impact", () => {
  assert.throws(() => releaseLabel("Add search"), /invalid pull request title/);
});
