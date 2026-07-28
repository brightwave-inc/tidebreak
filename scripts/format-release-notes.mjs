#!/usr/bin/env node

import { pathToFileURL } from "node:url";

import { parsePrTitle } from "./check-pr-title.mjs";

const SCOPED_CATEGORY_TITLES = new Set([
  "Breaking Changes",
  "New Features",
  "Bug Fixes",
  "Performance Improvements",
  "Dependency Updates",
  "Reverted Changes",
]);

const CHANGE_LINE_PATTERN = /^- (.*?) \(\[#\d+\]\([^)]*\)\) by @\S+$/;

const SCOPE_ACRONYMS = new Map([
  ["api", "API"],
  ["cli", "CLI"],
  ["db", "DB"],
  ["mcp", "MCP"],
  ["ui", "UI"],
  ["ux", "UX"],
]);

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
    formatted.push(`#### ${scopeHeading(scope)}`, ...scopeChanges);
  }
  if (flatLines.length > 0 && formatted.length > 0) {
    // A bare blank line between two bullet runs makes GitHub render both as
    // one loose list. A heading closes the grouped list before the compact
    // inline scope changes begin.
    formatted.push("", "#### Other", ...flatLines);
  } else {
    formatted.push(...flatLines);
  }
  return [...formatted, ...trailingLines];
}

export function formatReleaseNotes(body) {
  if (typeof body !== "string") {
    throw new Error("release notes must be a string");
  }

  const lines = body.split("\n");
  const formatted = [];
  for (let index = 0; index < lines.length; ) {
    const heading = /^### (.+)$/.exec(lines[index]);
    if (!heading || !SCOPED_CATEGORY_TITLES.has(heading[1])) {
      formatted.push(lines[index]);
      index += 1;
      continue;
    }

    formatted.push(lines[index]);
    const categoryStart = index + 1;
    index = categoryStart;
    while (index < lines.length && !/^### /.test(lines[index])) index += 1;
    formatted.push(...formatCategory(lines.slice(categoryStart, index)));
  }

  return formatted.join("\n");
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
