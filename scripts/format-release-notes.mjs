#!/usr/bin/env node

import { pathToFileURL } from "node:url";

import { parsePrTitle } from "./check-pr-title.mjs";

const THANK_YOU = `> ❤️ **Thanks for using Tidebreak.**
>
> If you filed an issue, reviewed a pull request, or shipped a change, you are in these notes.`;

const LEGACY_THANK_YOU = "Thanks for using Tidebreak ❤️";

const CATEGORIES = [
  {
    key: "Breaking Changes",
    title: "💥 Breaking Changes",
    scoped: true,
  },
  {
    key: "New Features",
    title: "✨ New Features",
    scoped: true,
  },
  {
    key: "Bug Fixes",
    title: "🐛 Bug Fixes",
    scoped: true,
  },
  {
    key: "Performance Improvements",
    title: "⚡ Performance Improvements",
    scoped: true,
  },
  {
    key: "Dependency Updates",
    title: "📦 Dependency Updates",
    scoped: true,
  },
  {
    key: "Reverted Changes",
    title: "⏪ Reverted Changes",
    scoped: true,
  },
  {
    key: "Maintenance",
    title: "🧰 Maintenance",
    scoped: false,
  },
  {
    key: "Other Changes",
    title: "📝 Other Changes",
    scoped: false,
  },
];

const CATEGORY_BY_KEY = new Map(
  CATEGORIES.flatMap((category) => [
    [category.key, category],
    [category.title, category],
  ]),
);

const CHANGE_LINE_PATTERN = /^- (.*?) \(\[#\d+\]\([^)]*\)\) by @\S+$/;

const SCOPE_ACRONYMS = new Map([
  ["api", "API"],
  ["cli", "CLI"],
  ["db", "DB"],
  ["mcp", "MCP"],
  ["ui", "UI"],
  ["ux", "UX"],
]);

const NEW_CONTRIBUTORS_KEY = "New Contributors";
const NEW_CONTRIBUTORS_TITLE = "🙌 New Contributors";
const EMPTY_NEW_CONTRIBUTOR_LINES = new Set([
  "",
  "- No new contributors",
  "* No new contributors",
]);

function headingKey(title) {
  return title.replace(/^[^\p{L}\p{N}]+/u, "").trim();
}

function scopeHeading(scope) {
  return scope
    .split(/([._/-])/)
    .map((part) => {
      if (/^[._/-]$/.test(part)) return part;
      if (SCOPE_ACRONYMS.has(part)) return SCOPE_ACRONYMS.get(part);
      return `${part[0].toUpperCase()}${part.slice(1)}`;
    })
    .join("");
}

function parseChangeLine(line) {
  const match = CHANGE_LINE_PATTERN.exec(line);
  if (!match) return null;

  const title = parsePrTitle(match[1]);
  if (!title) return null;

  return {
    scope: title.scope,
    line: line.replace(match[1], title.description),
  };
}

function inlineScope(scope, line) {
  return `- **${scopeHeading(scope)}:** ${line.slice(2)}`;
}

function formatCategory(lines) {
  let contentLength = lines.length;
  while (contentLength > 0 && lines[contentLength - 1] === "") {
    contentLength -= 1;
  }
  const contentLines = lines.slice(0, contentLength);
  const trailingLines = lines.slice(contentLength);
  const changes = contentLines.map(parseChangeLine);
  if (changes.every((change) => change === null)) return lines;

  const scopedChanges = new Map();
  const unscopedLines = [];
  for (let index = 0; index < contentLines.length; index += 1) {
    const change = changes[index];
    if (!change?.scope) {
      unscopedLines.push(change ? change.line : contentLines[index]);
      continue;
    }

    const group = scopedChanges.get(change.scope) ?? [];
    group.push(change.line);
    scopedChanges.set(change.scope, group);
  }

  const groupedScopes = [];
  const singletonLines = [];
  for (const scope of [...scopedChanges.keys()].sort()) {
    const scopeChanges = scopedChanges.get(scope);
    if (scopeChanges.length === 1) {
      singletonLines.push(inlineScope(scope, scopeChanges[0]));
      continue;
    }
    groupedScopes.push([scope, scopeChanges]);
  }

  const flatLines = [
    ...unscopedLines.filter((line) => line !== ""),
    ...singletonLines,
  ];
  const formatted = [];
  for (const [scope, scopeChanges] of groupedScopes) {
    if (formatted.length > 0) formatted.push("");
    formatted.push(`### ${scopeHeading(scope)}`, ...scopeChanges);
  }
  if (flatLines.length > 0 && formatted.length > 0) {
    // A bare blank line between two bullet runs makes GitHub render both as
    // one loose list. A heading closes the grouped list before the compact
    // inline scope changes begin.
    formatted.push("", "### Other", ...flatLines);
  } else {
    formatted.push(...flatLines);
  }
  return [...formatted, ...trailingLines];
}

function stripLeadingThankYou(lines) {
  if (lines[0] === LEGACY_THANK_YOU) {
    lines = lines.slice(1);
    if (lines[0] === "") lines = lines.slice(1);
    return lines;
  }

  const thankYouLines = THANK_YOU.split("\n");
  if (
    thankYouLines.every((line, index) => lines[index] === line)
  ) {
    lines = lines.slice(thankYouLines.length);
    if (lines[0] === "") lines = lines.slice(1);
  }
  return lines;
}

function dropEmptyNewContributors(lines) {
  const headingIndex = lines.findIndex((line) => {
    const heading = /^## (.+)$/.exec(line);
    return heading && headingKey(heading[1]) === NEW_CONTRIBUTORS_KEY;
  });
  if (headingIndex === -1) return lines;

  let end = headingIndex + 1;
  while (end < lines.length && !/^## /.test(lines[end]) && !/^\*\*/.test(lines[end])) {
    end += 1;
  }
  const content = lines.slice(headingIndex + 1, end);
  if (content.every((line) => EMPTY_NEW_CONTRIBUTOR_LINES.has(line))) {
    const before = lines.slice(0, headingIndex);
    const after = lines.slice(end);
    while (before.at(-1) === "") before.pop();
    if (after[0] !== "" && after.length > 0 && before.length > 0) {
      return [...before, "", ...after];
    }
    return [...before, ...after];
  }

  lines[headingIndex] = `## ${NEW_CONTRIBUTORS_TITLE}`;
  return lines;
}

export function formatReleaseNotes(body) {
  if (typeof body !== "string") {
    throw new Error("release notes must be a string");
  }

  let lines = stripLeadingThankYou(body.split("\n"));
  const formatted = [];
  for (let index = 0; index < lines.length; ) {
    if (/^#{1,2} What's Changed$/.test(lines[index])) {
      index += 1;
      if (lines[index] === "") index += 1;
      continue;
    }

    const heading = /^#{2,3} (.+)$/.exec(lines[index]);
    const key = heading ? headingKey(heading[1]) : null;
    const category = key ? CATEGORY_BY_KEY.get(key) : null;
    if (!category) {
      formatted.push(lines[index]);
      index += 1;
      continue;
    }

    formatted.push(`## ${category.title}`);
    const categoryStart = index + 1;
    index = categoryStart;
    while (index < lines.length) {
      const nextHeading = /^#{2,3} (.+)$/.exec(lines[index]);
      const nextKey = nextHeading ? headingKey(nextHeading[1]) : null;
      if (nextKey && CATEGORY_BY_KEY.has(nextKey)) break;
      if (nextKey === NEW_CONTRIBUTORS_KEY) break;
      index += 1;
    }

    const categoryLines = lines.slice(categoryStart, index);
    formatted.push(
      ...(category.scoped && !categoryLines.some((line) => /^### /.test(line))
        ? formatCategory(categoryLines)
        : categoryLines),
    );
  }

  const bodyLines = dropEmptyNewContributors(formatted);
  while (bodyLines[0] === "") bodyLines.shift();
  return `${THANK_YOU}\n\n${bodyLines.join("\n")}`;
}

async function main() {
  let body = "";
  for await (const chunk of process.stdin) body += chunk;
  process.stdout.write(formatReleaseNotes(body));
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  await main();
}
