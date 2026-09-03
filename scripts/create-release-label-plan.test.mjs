import assert from "node:assert/strict";
import test from "node:test";

import { createReleaseLabelPlan } from "./create-release-label-plan.mjs";

test("plans exact title-derived labels without touching unrelated labels", () => {
  const plan = createReleaseLabelPlan([
    {
      number: 12,
      title: "feat(core): add search",
      labels: [{ name: "area:core" }, { name: "semver:patch" }],
    },
  ]);

  assert.deepEqual(plan.summary, {
    total: 1,
    classified: 1,
    updates: 1,
    unchanged: 0,
    skipped: 0,
  });
  assert.deepEqual(plan.updates[0], {
    number: 12,
    title: "feat(core): add search",
    desired_labels: ["release-note:feature", "semver:minor"],
    add_labels: ["release-note:feature", "semver:minor"],
    remove_labels: ["semver:patch"],
  });
});

test("leaves already-correct labels unchanged", () => {
  const plan = createReleaseLabelPlan([
    {
      number: 13,
      title: "fix: prevent a race",
      labels: ["release-note:fix", "semver:patch", "area:core"],
    },
  ]);

  assert.equal(plan.summary.unchanged, 1);
  assert.deepEqual(plan.updates, []);
});

test("plans improvement release labels", () => {
  const plan = createReleaseLabelPlan([
    {
      number: 14,
      title: "improve(desktop): simplify navigation",
      labels: ["release-note:feature", "semver:minor"],
    },
  ]);

  assert.deepEqual(plan.updates[0], {
    number: 14,
    title: "improve(desktop): simplify navigation",
    desired_labels: ["release-note:improvement", "semver:patch"],
    add_labels: ["release-note:improvement", "semver:patch"],
    remove_labels: ["release-note:feature", "semver:minor"],
  });
});

test("skips ambiguous historical titles instead of guessing", () => {
  const plan = createReleaseLabelPlan([
    { number: 15, title: "Add search", labels: [] },
  ]);

  assert.equal(plan.summary.skipped, 1);
  assert.deepEqual(plan.updates, []);
  assert.match(plan.skipped[0].reason, /invalid pull request title/);
});

test("rejects malformed pull request input", () => {
  assert.throws(
    () => createReleaseLabelPlan([{ number: 0, title: "feat: add search", labels: [] }]),
    /invalid pull request number/,
  );
});
