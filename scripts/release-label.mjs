#!/usr/bin/env node

import { pathToFileURL } from "node:url";

import { parsePrTitle, validatePrTitle } from "./check-pr-title.mjs";

export const MANAGED_RELEASE_LABELS = [
  "semver:breaking",
  "semver:minor",
  "semver:patch",
  "semver:none",
  "release-note:feature",
  "release-note:improvement",
  "release-note:fix",
  "release-note:performance",
  "release-note:dependencies",
  "release-note:revert",
];

const PATCH_TYPES = new Set(["improve", "fix", "perf", "deps", "revert"]);
const RELEASE_NOTE_LABELS = new Map([
  ["feat", "release-note:feature"],
  ["improve", "release-note:improvement"],
  ["fix", "release-note:fix"],
  ["perf", "release-note:performance"],
  ["deps", "release-note:dependencies"],
  ["revert", "release-note:revert"],
]);

export function releaseLabel(title) {
  const error = validatePrTitle(title);
  if (error) throw new Error(error);
  const parsed = parsePrTitle(title);
  if (parsed.breaking) return "semver:breaking";
  if (parsed.type === "feat") return "semver:minor";
  if (PATCH_TYPES.has(parsed.type)) return "semver:patch";
  return "semver:none";
}

export function releaseNoteLabel(title) {
  const error = validatePrTitle(title);
  if (error) throw new Error(error);
  const parsed = parsePrTitle(title);
  if (parsed.breaking) return null;
  return RELEASE_NOTE_LABELS.get(parsed.type) ?? null;
}

export function releaseLabels(title) {
  return [releaseLabel(title), releaseNoteLabel(title)].filter(Boolean);
}

function main() {
  const [mode, ...rest] = process.argv.slice(2);
  if (mode === "--managed") {
    console.log(JSON.stringify(MANAGED_RELEASE_LABELS));
    return;
  }
  if (!["--title", "--labels"].includes(mode) || rest.length === 0) {
    throw new Error(
      "usage: release-label.mjs --title <PR title> | --labels <PR title> | --managed",
    );
  }
  const title = rest.join(" ");
  console.log(
    mode === "--labels"
      ? JSON.stringify(releaseLabels(title))
      : releaseLabel(title),
  );
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
