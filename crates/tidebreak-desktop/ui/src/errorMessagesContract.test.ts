import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * Every failure a reader sees is worded by one formatter,
 * `friendlyErrorMessage` in lib/utils.ts. `String(err)` is how an exception's
 * class name and a status code used to reach the screen ("HttpError: 409: …",
 * "TypeError: Load failed"), so it has no place in UI state.
 */
const STRINGIFIED_ERROR =
  /\bString\(\s*(?:err|error|e|reason|caught|cause|failure|ex|exception)\s*\)/;

/** Where turning a caught value into text is not the error UI, and why. */
const ALLOWLIST: ReadonlyMap<string, string> = new Map([
  ["lib/utils.ts", "The formatter itself turns an unknown value into text."],
  [
    "rendererErrors.ts",
    "A report for this machine's log, never shown; it scrubs on its own.",
  ],
  [
    "McpAppBridge.ts",
    "A JSON-RPC error handed back to an MCP app, not text a reader sees.",
  ],
  [
    "BootFailure.tsx",
    "The boot screen keeps the raw error under its own headline as the evidence a bug report needs.",
  ],
]);

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

function stringifiedErrors(): string[] {
  const everywhere = new RegExp(STRINGIFIED_ERROR.source, "g");
  const hits: string[] = [];
  for (const file of sourceFiles()) {
    if (ALLOWLIST.has(file)) continue;
    const lines = readFileSync(join(SRC, file), "utf8").split("\n");
    lines.forEach((line, index) => {
      for (const match of line.matchAll(everywhere)) {
        hits.push(`${file}:${index + 1}  ${match[0]}`);
      }
    });
  }
  return hits;
}

describe("error messages contract", () => {
  it("words every failure through friendlyErrorMessage, never String(err)", () => {
    expect(
      stringifiedErrors(),
      "Show a caught value with friendlyErrorMessage(err, fallback) from " +
        "lib/utils.ts. It drops the class name and the status code, words " +
        "network failures, and prefers renderer copy for known kinds. " +
        "Allowlist a use that never reaches the screen here, with a reason.",
    ).toEqual([]);
  });

  it("recognizes the ways a caught value was stringified", () => {
    for (const line of [
      "setError(String(err));",
      "`Could not load work: ${String(err)}`",
      "set({ error: String(caught) });",
      "if (!cancelled) setError(String(reason));",
    ]) {
      expect(STRINGIFIED_ERROR.test(line), line).toBe(true);
    }
    // Numbers and ids are still text; only caught values are banned.
    expect(STRINGIFIED_ERROR.test("String(count)")).toBe(false);
  });

  it("allowlists only files that exist", () => {
    const files = new Set(sourceFiles());
    expect([...ALLOWLIST.keys()].filter((file) => !files.has(file))).toEqual(
      [],
    );
  });
});
