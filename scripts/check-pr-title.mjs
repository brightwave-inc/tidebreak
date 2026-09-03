#!/usr/bin/env node

import { pathToFileURL } from "node:url";

export const ALLOWED_TYPES = [
  "feat",
  "improve",
  "fix",
  "perf",
  "deps",
  "revert",
  "docs",
  "refactor",
  "chore",
  "build",
  "ci",
  "test",
];

const RELEASING_TYPES = new Set([
  "feat",
  "improve",
  "fix",
  "perf",
  "deps",
  "revert",
]);

const TITLE_PATTERN = new RegExp(
  `^(${ALLOWED_TYPES.join("|")})(?:\\(([a-z0-9]+(?:[._/-][a-z0-9]+)*)\\))?(!)?: (\\S.*)$`,
);

export function parsePrTitle(title) {
  if (typeof title !== "string") return null;
  const match = TITLE_PATTERN.exec(title);
  if (!match) return null;
  return {
    type: match[1],
    scope: match[2] ?? null,
    breaking: Boolean(match[3]),
    description: match[4],
  };
}

export function validatePrTitle(title, body = "") {
  if (typeof title !== "string" || title.length === 0) {
    return "the pull request title is empty";
  }

  const parsed = parsePrTitle(title);
  if (!parsed) {
    return [
      `invalid pull request title: ${JSON.stringify(title)}`,
      "expected: type(optional-scope)[!]: description",
      `allowed types: ${ALLOWED_TYPES.join(", ")}`,
      "examples: feat(desktop): add search, improve(desktop): simplify navigation, fix(core): prevent a race",
    ].join("\n");
  }

  const { type, breaking } = parsed;
  if (breaking && !RELEASING_TYPES.has(type)) {
    return [
      `invalid breaking pull request title: ${JSON.stringify(title)}`,
      "the ! marker must use a release-driving type: feat, improve, fix, perf, deps, or revert",
      "classify a breaking refactor or build change by its user impact, usually feat!, improve!, or fix!",
    ].join("\n");
  }

  const hasBreakingFooter = /^(?:BREAKING CHANGE|BREAKING-CHANGE):\s*\S/im.test(
    body,
  );
  if (hasBreakingFooter && !breaking) {
    return [
      "the pull request body contains a breaking-change footer but the title has no ! marker",
      "put ! before the colon in the semantic title so the release impact is visible during review",
    ].join("\n");
  }

  return null;
}

function main() {
  const title = process.argv.slice(2).join(" ");
  const error = validatePrTitle(title, process.env.PR_BODY ?? "");
  if (error) {
    console.error(error);
    process.exitCode = 1;
  } else {
    console.log(`valid semantic pull request title: ${title}`);
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
