import assert from "node:assert/strict";
import test from "node:test";

import {
  evaluateEngineeringApproval,
  hasWritePermission,
} from "./check-engineering-approval.mjs";

const permissions = {
  naingthet: "admin",
  bkotara: "write",
  vvnlo: "read",
  outsider: "read",
};

function decide(overrides) {
  return evaluateEngineeringApproval({
    eventName: "pull_request_target",
    author: "outsider",
    headSha: "abc123def456",
    reviews: [],
    permissionFor: (login) => permissions[login] ?? "none",
    ...overrides,
  });
}

test("write permissions are admin, maintain, and write", () => {
  assert.equal(hasWritePermission("admin"), true);
  assert.equal(hasWritePermission("maintain"), true);
  assert.equal(hasWritePermission("write"), true);
  assert.equal(hasWritePermission("read"), false);
  assert.equal(hasWritePermission("none"), false);
  assert.equal(hasWritePermission("triage"), false);
});

test("merge groups pass without looking up reviewers", () => {
  const decision = decide({
    eventName: "merge_group",
    author: "",
    headSha: "",
    permissionFor: () => {
      throw new Error("merge groups must not look up permissions");
    },
  });
  assert.equal(decision.ok, true);
});

test("a writer can merge their own pull request without a review", () => {
  const decision = decide({ author: "naingthet" });
  assert.equal(decision.ok, true);
  assert.match(decision.reason, /naingthet/);
});

test("a read-only org member still needs a writer approval", () => {
  const decision = decide({ author: "vvnlo" });
  assert.equal(decision.ok, false);
});

test("an outsider with no reviews cannot merge", () => {
  const decision = decide({});
  assert.equal(decision.ok, false);
  assert.match(decision.reason, /outsider/);
});

test("an outsider needs a writer approval on the current head", () => {
  const decision = decide({
    reviews: [
      { state: "APPROVED", commitId: "abc123def456", user: "bkotara" },
    ],
  });
  assert.equal(decision.ok, true);
  assert.match(decision.reason, /bkotara/);
});

test("an approval of an older commit does not count", () => {
  const decision = decide({
    reviews: [
      { state: "APPROVED", commitId: "000000000000", user: "bkotara" },
    ],
  });
  assert.equal(decision.ok, false);
});

test("a read-only approval does not count", () => {
  const decision = decide({
    reviews: [
      { state: "APPROVED", commitId: "abc123def456", user: "vvnlo" },
    ],
  });
  assert.equal(decision.ok, false);
});

test("a comment or dismissed review does not count", () => {
  for (const state of ["COMMENTED", "DISMISSED", "CHANGES_REQUESTED"]) {
    const decision = decide({
      reviews: [{ state, commitId: "abc123def456", user: "bkotara" }],
    });
    assert.equal(decision.ok, false, state);
  }
});

test("a later changes-requested review replaces an approval", () => {
  const decision = decide({
    reviews: [
      { state: "APPROVED", commitId: "abc123def456", user: "bkotara" },
      {
        state: "CHANGES_REQUESTED",
        commitId: "abc123def456",
        user: "bkotara",
      },
    ],
  });
  assert.equal(decision.ok, false);
});

test("a later approval on the current head replaces an older review", () => {
  const decision = decide({
    reviews: [
      { state: "CHANGES_REQUESTED", commitId: "000000000000", user: "bkotara" },
      { state: "APPROVED", commitId: "abc123def456", user: "bkotara" },
    ],
  });
  assert.equal(decision.ok, true);
});

test("a missing author or head SHA fails closed", () => {
  assert.equal(decide({ author: "" }).ok, false);
  assert.equal(decide({ headSha: "" }).ok, false);
});
