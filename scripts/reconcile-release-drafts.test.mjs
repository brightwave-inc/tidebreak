import assert from "node:assert/strict";
import test from "node:test";

import {
  normalizeReleaseList,
  planDraftReconciliation,
} from "./reconcile-release-drafts.mjs";

const published = {
  id: 1,
  tag_name: "v0.46.2",
  draft: false,
  prerelease: false,
};
const currentDraft = {
  id: 20,
  tag_name: "v0.47.0",
  draft: true,
  prerelease: false,
};
const staleDraft = {
  id: 10,
  tag_name: "v0.47.0",
  draft: true,
  prerelease: false,
};
const firstReleaseDraft = {
  id: 30,
  tag_name: "v0.0.1",
  draft: true,
  prerelease: false,
};
const inFlight = {
  id: 40,
  tag_name: "v0.47.0",
  draft: false,
  prerelease: true,
};

test("keeps the single next-version draft", () => {
  assert.deepEqual(
    planDraftReconciliation({
      releases: [currentDraft, published],
      resolvedVersion: "0.47.0",
      releaseId: 20,
    }),
    {
      action: "ok",
      keep_id: 20,
      delete_ids: [],
      reason: "kept the single draft v0.47.0",
    },
  );
});

test("removes extra drafts after a successful draft update", () => {
  assert.deepEqual(
    planDraftReconciliation({
      releases: [currentDraft, firstReleaseDraft, staleDraft, published],
      resolvedVersion: "0.47.0",
      releaseId: 20,
    }),
    {
      action: "ok",
      keep_id: 20,
      delete_ids: [30, 10],
      reason: "kept v0.47.0 and removed 2 extra draft(s)",
    },
  );
});

test("rejects a lost baseline and deletes only the v0.0.1 drafts", () => {
  assert.deepEqual(
    planDraftReconciliation({
      releases: [firstReleaseDraft, currentDraft, published],
      resolvedVersion: "0.0.1",
      releaseId: 30,
    }),
    {
      action: "fail",
      keep_id: 20,
      delete_ids: [30],
      reason:
        "Release Drafter resolved v0.0.1 even though published releases exist",
    },
  );
});

test("allows the genuine first-release v0.0.1 draft", () => {
  assert.deepEqual(
    planDraftReconciliation({
      releases: [firstReleaseDraft],
      resolvedVersion: "0.0.1",
      releaseId: 30,
    }),
    {
      action: "ok",
      keep_id: 30,
      delete_ids: [],
      reason: "kept the single draft v0.0.1",
    },
  );
});

test("ignores an in-flight prerelease when collapsing drafts", () => {
  const nextDraft = {
    id: 50,
    tag_name: "v0.48.0",
    draft: true,
    prerelease: false,
  };
  assert.deepEqual(
    planDraftReconciliation({
      releases: [nextDraft, inFlight, published],
      resolvedVersion: "0.48.0",
      releaseId: 50,
    }),
    {
      action: "ok",
      keep_id: 50,
      delete_ids: [],
      reason: "kept the single draft v0.48.0",
    },
  );
});

test("ignores an in-flight draft prerelease when collapsing drafts", () => {
  const nextDraft = {
    id: 50,
    tag_name: "v0.51.0",
    draft: true,
    prerelease: false,
  };
  const inFlightDraft = {
    id: 60,
    tag_name: "v0.50.0",
    draft: true,
    prerelease: true,
  };
  assert.deepEqual(
    planDraftReconciliation({
      releases: [nextDraft, inFlightDraft, published],
      resolvedVersion: "0.51.0",
      releaseId: 50,
    }),
    {
      action: "ok",
      keep_id: 50,
      delete_ids: [],
      reason: "kept the single draft v0.51.0",
    },
  );
});

test("flattens paginated GitHub release pages", () => {
  assert.deepEqual(normalizeReleaseList([[currentDraft], [published]]), [
    currentDraft,
    published,
  ]);
});

test("retries when GitHub has not listed the new Release Drafter id yet", () => {
  assert.deepEqual(
    planDraftReconciliation({
      releases: [published],
      resolvedVersion: "0.47.0",
      releaseId: 20,
    }),
    {
      action: "retry",
      keep_id: null,
      delete_ids: [],
      reason:
        "Release Drafter reported release 20, but the releases list does not include it yet",
    },
  );
});

test("rejects a Release Drafter id that is listed but is not a live draft", () => {
  assert.throws(
    () =>
      planDraftReconciliation({
        releases: [{ ...inFlight, id: 20 }, published],
        resolvedVersion: "0.47.0",
        releaseId: 20,
      }),
    /not a non-prerelease draft/,
  );
});
