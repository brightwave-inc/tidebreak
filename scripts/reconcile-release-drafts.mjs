#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const FIRST_RELEASE_VERSION = "0.0.1";

function requirePositiveInteger(value, label) {
  const parsed = typeof value === "number" ? value : Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`invalid ${label}: ${value}`);
  }
  return parsed;
}

export function normalizeReleaseList(input) {
  if (!Array.isArray(input)) {
    throw new Error("releases must be a JSON array");
  }
  return input.flatMap((entry) => {
    if (Array.isArray(entry)) return entry;
    return [entry];
  });
}

export function planDraftReconciliation({
  releases,
  resolvedVersion,
  releaseId,
}) {
  if (typeof resolvedVersion !== "string" || resolvedVersion.length === 0) {
    throw new Error("resolved version is required");
  }

  const resolvedReleaseId = requirePositiveInteger(releaseId, "release id");
  const allReleases = normalizeReleaseList(releases);
  const drafts = allReleases.filter(
    (release) => release?.draft === true && release?.prerelease === false,
  );
  const published = allReleases.filter((release) => release?.draft === false);

  const lostBaseline =
    resolvedVersion === FIRST_RELEASE_VERSION && published.length > 0;
  if (lostBaseline) {
    return {
      action: "fail",
      keep_id:
        drafts.find((draft) => draft.tag_name !== `v${FIRST_RELEASE_VERSION}`)
          ?.id ?? null,
      delete_ids: drafts
        .filter((draft) => draft.tag_name === `v${FIRST_RELEASE_VERSION}`)
        .map((draft) => draft.id),
      reason:
        "Release Drafter resolved v0.0.1 even though published releases exist",
    };
  }

  const keep = drafts.find((draft) => draft.id === resolvedReleaseId);
  if (!keep) {
    throw new Error(
      `Release Drafter reported release ${resolvedReleaseId} but it is not a non-prerelease draft`,
    );
  }

  const deleteIds = drafts
    .filter((draft) => draft.id !== keep.id)
    .map((draft) => draft.id);
  return {
    action: "ok",
    keep_id: keep.id,
    delete_ids: deleteIds,
    reason:
      deleteIds.length === 0
        ? `kept the single draft ${keep.tag_name}`
        : `kept ${keep.tag_name} and removed ${deleteIds.length} extra draft(s)`,
  };
}

function parseArgs(argv) {
  const args = {
    releases: null,
    resolvedVersion: null,
    releaseId: null,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (flag === "--releases") {
      args.releases = value;
      index += 1;
    } else if (flag === "--resolved-version") {
      args.resolvedVersion = value;
      index += 1;
    } else if (flag === "--release-id") {
      args.releaseId = value;
      index += 1;
    } else {
      throw new Error(`unknown argument: ${flag}`);
    }
  }
  if (!args.releases || !args.resolvedVersion || !args.releaseId) {
    throw new Error(
      "usage: reconcile-release-drafts.mjs --releases <path> --resolved-version <version> --release-id <id>",
    );
  }
  return args;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const plan = planDraftReconciliation({
    releases: JSON.parse(readFileSync(args.releases, "utf8")),
    resolvedVersion: args.resolvedVersion,
    releaseId: args.releaseId,
  });
  console.log(JSON.stringify(plan));
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
