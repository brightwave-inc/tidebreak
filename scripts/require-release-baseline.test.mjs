import assert from "node:assert/strict";
import test from "node:test";

import {
  planReleaseBaseline,
  versionTagsFromRefs,
} from "./require-release-baseline.mjs";

const published = {
  id: 1,
  tag_name: "v0.46.2",
  draft: false,
  prerelease: false,
};
const draft = {
  id: 2,
  tag_name: "v0.47.0",
  draft: true,
  prerelease: false,
};
const inFlight = {
  id: 3,
  tag_name: "v0.47.0",
  draft: false,
  prerelease: true,
};

test("allows drafting when a published release is listed", () => {
  assert.deepEqual(
    planReleaseBaseline({
      releases: [draft, published],
      tags: ["refs/tags/v0.46.2"],
    }),
    {
      action: "ok",
      published_count: 1,
      version_tag_count: 1,
    },
  );
});

test("treats an in-flight prerelease as a published baseline", () => {
  assert.equal(
    planReleaseBaseline({
      releases: [inFlight],
      tags: ["v0.47.0"],
    }).action,
    "ok",
  );
});

test("refuses to draft when version tags exist but no release is listed", () => {
  assert.deepEqual(
    planReleaseBaseline({
      releases: [draft],
      tags: ["refs/tags/v0.46.2", "refs/tags/v0.46.2^{}"],
    }),
    {
      action: "fail",
      published_count: 0,
      version_tag_count: 1,
      reason:
        "version tags exist but no published GitHub Release was listed; refusing to invoke Release Drafter",
    },
  );
});

test("allows the genuine first-release path when history is empty", () => {
  assert.deepEqual(planReleaseBaseline({ releases: [], tags: [] }), {
    action: "first_release",
    published_count: 0,
    version_tag_count: 0,
  });
});

test("peels annotated tag refs and ignores non-release tags", () => {
  assert.deepEqual(
    versionTagsFromRefs([
      "refs/tags/v0.46.2",
      "refs/tags/v0.46.2^{}",
      "refs/tags/v0.0.0",
      "refs/tags/nightly",
      "v0.47.0",
    ]),
    ["v0.46.2", "v0.47.0"],
  );
});
