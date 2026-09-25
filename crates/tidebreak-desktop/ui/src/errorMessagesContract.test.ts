import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * Every failure a reader sees is worded by one formatter,
 * `friendlyErrorMessage` in lib/utils.ts. Turning a caught value into text
 * by hand is how an exception's class name and a status code used to reach
 * the screen ("HttpError: 409: …", "TypeError: Load failed"), so none of the
 * shapes below may appear in the app.
 *
 * A line that turns a caught value into text for something other than the
 * screen (a log, a protocol reply, a copied report) says so with a
 * `raw-error-ok: <reason>` comment on the line or the line above it.
 */

/** Names a caught value goes by, including `loadError`-style state. */
const ERROR_NAME =
  "(?:err|error|e|ex|reason|caught|cause|failure|exception|[a-z]\\w*(?:Error|Err|Failure|Exception|Reason))";

/** Shapes that stringify a value, whatever the file calls it. */
const STRINGIFIED: ReadonlyArray<[string, RegExp]> = [
  ["String(err)", new RegExp(`\\bString\\(\\s*${ERROR_NAME}\\s*\\)`)],
  ["err.toString()", new RegExp(`\\b${ERROR_NAME}\\??\\.toString\\(\\)`)],
  [
    "JSON.stringify(err)",
    new RegExp(`\\bJSON\\.stringify\\(\\s*${ERROR_NAME}\\s*[,)]`),
  ],
  [
    "err instanceof Error ? err.message",
    new RegExp(
      `\\b(${ERROR_NAME})\\s+instanceof\\s+Error\\s*\\?\\s*\\1\\.message\\b`,
    ),
  ],
];

/**
 * Shapes that are only a problem when the name holds a caught value, since a
 * component's `error` state is often already worded: interpolating it into a
 * template, or concatenating it onto a string.
 */
function interpolated(name: string): ReadonlyArray<[string, RegExp]> {
  return [
    ["`${err}`", new RegExp(`\\$\\{\\s*${name}\\s*\\}`)],
    [
      '"…" + err',
      new RegExp(
        `(?:["'\`]\\s*\\+\\s*${name}\\b(?!\\s*[.([])|\\b${name}\\s*\\+\\s*["'\`])`,
      ),
    ],
  ];
}

/** Names this file binds to a caught value, plus the ones that always are. */
function caughtNames(text: string): string[] {
  const names = new Set(["err", "caught"]);
  for (const binding of [/\bcatch\s*\(\s*(\w+)/g, /\.catch\(\s*\(?\s*(\w+)/g]) {
    for (const match of text.matchAll(binding)) names.add(match[1]);
  }
  return [...names];
}

const EXCEPTION = /raw-error-ok:\s*(\S.*)$/;
/** An exception names a reason, not just the marker. */
const MIN_REASON_WORDS = 3;

const SRC = join(import.meta.dirname, ".");

function sourceFiles(): string[] {
  return readdirSync(SRC, { recursive: true, encoding: "utf8" })
    .map((path) => path.replaceAll("\\", "/"))
    .filter(
      (path) =>
        /\.(ts|tsx)$/.test(path) &&
        !/\.test\.(ts|tsx)$/.test(path) &&
        !path.includes("generated/"),
    );
}

/** Prose about a pattern is not the pattern. */
function isComment(line: string): boolean {
  return /^\s*(?:\/\/|\/?\*)/.test(line);
}

function excepted(lines: string[], index: number): boolean {
  return [lines[index], lines[index - 1] ?? ""].some((line) => {
    const reason = EXCEPTION.exec(line)?.[1] ?? "";
    return reason.trim().split(/\s+/).length >= MIN_REASON_WORDS;
  });
}

function offenders(text: string, file: string): string[] {
  const lines = text.split("\n");
  const patterns = [
    ...STRINGIFIED,
    ...caughtNames(text).flatMap((name) => interpolated(name)),
  ];
  const hits: string[] = [];
  lines.forEach((line, index) => {
    if (isComment(line) || excepted(lines, index)) return;
    for (const [shape, pattern] of patterns) {
      if (pattern.test(line)) {
        hits.push(`${file}:${index + 1}  ${shape}  ${line.trim()}`);
        return;
      }
    }
  });
  return hits;
}

describe("error messages contract", () => {
  it("words every failure through friendlyErrorMessage, never by hand", () => {
    const hits = sourceFiles().flatMap((file) =>
      offenders(readFileSync(join(SRC, file), "utf8"), file),
    );
    expect(
      hits,
      "Show a caught value with friendlyErrorMessage(err, fallback) from " +
        "lib/utils.ts. It drops the class name and the status code, words " +
        "network failures, and keeps the server's message. A line that " +
        "stringifies a caught value for a log or a protocol, never the " +
        "screen, says why with a `raw-error-ok: <reason>` comment.",
    ).toEqual([]);
  });

  it("recognizes every hand-made spelling of a caught value", () => {
    const flagged = (line: string) =>
      offenders(`try {} catch (reason) {}\n${line}`, "probe.ts").length > 0;
    for (const line of [
      "setError(String(err));",
      "set({ error: String(caught) });",
      "setError(String(loadError));",
      "setError(err.toString());",
      "setError(JSON.stringify(error));",
      "setError(error instanceof Error ? error.message : 'x');",
      "toast.error(`Could not load: ${err}`);",
      "toast.error(`Could not load: ${reason}`);",
      'setError("Could not load: " + caught);',
      "setError(reason + ' (retrying)');",
    ]) {
      expect(flagged(line), line).toBe(true);
    }
  });

  it("leaves worded state and unrelated values alone", () => {
    const flagged = (line: string) => offenders(line, "probe.tsx").length > 0;
    for (const line of [
      // `error` here is state a formatter already worded.
      "<p>{`Could not attach: ${error}`}</p>",
      "String(count)",
      "const label = `${failureCount} failed`;",
      " * `String(err)` would show the class name.",
      "reportLog(String(error)); // raw-error-ok: the renderer log, never shown",
    ]) {
      expect(flagged(line), line).toBe(false);
    }
  });

  it("accepts an exception only with a reason", () => {
    const line = "log(String(error)); // raw-error-ok:";
    expect(offenders(line, "probe.ts")).toHaveLength(1);
    expect(
      offenders(`${line} a log line, never shown`, "probe.ts"),
    ).toHaveLength(0);
  });
});
