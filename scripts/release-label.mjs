#!/usr/bin/env node

import { pathToFileURL } from "node:url";

import { parsePrTitle, validatePrTitle } from "./check-pr-title.mjs";

export const MANAGED_RELEASE_LABELS = [
  "semver:breaking",
  "semver:minor",
  "semver:patch",
  "semver:none",
];

const PATCH_TYPES = new Set(["fix", "perf", "deps", "revert"]);

export function releaseLabel(title) {
  const error = validatePrTitle(title);
  if (error) throw new Error(error);
  const parsed = parsePrTitle(title);
  if (parsed.breaking) return "semver:breaking";
  if (parsed.type === "feat") return "semver:minor";
  if (PATCH_TYPES.has(parsed.type)) return "semver:patch";
  return "semver:none";
}

function main() {
  const [mode, ...rest] = process.argv.slice(2);
  if (mode === "--managed") {
    console.log(JSON.stringify(MANAGED_RELEASE_LABELS));
    return;
  }
  if (mode !== "--title" || rest.length === 0) {
    throw new Error("usage: release-label.mjs --title <PR title> | --managed");
  }
  console.log(releaseLabel(rest.join(" ")));
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
