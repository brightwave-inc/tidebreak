import assert from "node:assert/strict";
import test from "node:test";

import {
  MANAGED_RELEASE_LABELS,
  releaseLabel,
  releaseLabels,
  releaseNoteLabel,
} from "./release-label.mjs";

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

for (const [title, label] of [
  ["feat: add search", "release-note:feature"],
  ["fix(core): prevent a race", "release-note:fix"],
  ["perf: reduce startup time", "release-note:performance"],
  ["deps(cargo): update sqlite", "release-note:dependencies"],
  ["revert: restore startup behavior", "release-note:revert"],
  ["feat(core)!: replace the API", null],
  ["docs: explain search", null],
]) {
  test(`maps ${title} to release-note label ${label}`, () => {
    assert.equal(releaseNoteLabel(title), label);
  });
}

test("returns the exact managed labels expected on a pull request", () => {
  assert.deepEqual(releaseLabels("feat: add search"), [
    "semver:minor",
    "release-note:feature",
  ]);
  assert.deepEqual(releaseLabels("fix(core)!: replace the API"), [
    "semver:breaking",
  ]);
  assert.deepEqual(releaseLabels("ci: update checkout"), ["semver:none"]);
});

test("managed release labels are unique", () => {
  assert.equal(
    new Set(MANAGED_RELEASE_LABELS).size,
    MANAGED_RELEASE_LABELS.length,
  );
});
