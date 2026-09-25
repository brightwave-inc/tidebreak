import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * One name for each thing a person reads or hears about (DESIGN.md, Words).
 * The record is a conversation: "Work" names the mode and its navigation,
 * never one of the conversations in it. A coding agent runs in an engine:
 * "harness" stays in identifiers, types, file names, routes, and wire
 * values, where no one reads it. If a failure here is a false alarm, narrow
 * the pattern and add the line to the cases below; do not reword the copy
 * until the pattern misses it.
 */

const SRC = join(import.meta.dirname, ".");

/**
 * Product source only. Tests, stories, and benches hold fixtures, and
 * generated files hold wire names; none of them is copy.
 */
function sourceFiles(): string[] {
  return readdirSync(SRC, { recursive: true, encoding: "utf8" })
    .map((path) => path.replaceAll("\\", "/"))
    .filter(
      (path) =>
        /\.(ts|tsx)$/.test(path) &&
        !/\.(test|stories|bench)\.(ts|tsx)$/.test(path) &&
        !/\.d\.ts$/.test(path) &&
        !/(?:^|\/)(?:generated|stories|test)\//.test(path),
    );
}

/**
 * "Work" as a count noun for one conversation: "Delete work", "this work's
 * usage", "agents per work". Lowercase only, so the mode keeps its name
 * ("the Work list"), and a verb or the mass noun ("work on this computer",
 * "background work") never follows these words.
 */
const WORK_AS_COUNT_NOUN =
  /\b(?:[Tt]his|[Tt]he|[Pp]er|[Dd]elete|[Rr]ename)\s+work\b(?!\s+items?\b)/;

/** The engine's internal name, in any case, singular or plural. */
const HARNESS = /\bharness(?:es)?\b/i;

/** Quoted strings and template literals on one line. */
const QUOTED = /"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'|`((?:[^`\\]|\\.)*)`/g;

/** JSX text between a closing `>` and the next `<` on the same line. */
const JSX_TEXT_INLINE = />([^<>{}]+)</g;

/**
 * Marks code needs and prose does not: operators, member access, a type
 * assertion. Text carrying one is an expression, not JSX text.
 */
const CODE_MARKS =
  /[=;{}<>()`"]|\w\.\w|&&|\|\||\?\?|\?\.|\s[?:]\s|\bas\s+[A-Z]/;

/** A line that is JSX text by itself, such as a paragraph's second line. */
function isJsxTextLine(line: string): boolean {
  const text = line.trim();
  return (
    /^[A-Za-z]/.test(text) &&
    /\s/.test(text) &&
    !CODE_MARKS.test(text) &&
    !/^[\w$]+\??:/.test(text)
  );
}

/** Text a reader could meet: capitalized, or more than one word. */
function readsAsCopy(text: string): boolean {
  return (
    /[A-Za-z]/.test(text) && (/^\s*[A-Z]/.test(text) || /\S\s+\S/.test(text))
  );
}

/** The copy on one line: quoted strings, JSX text, and nothing else. */
function copyOn(line: string): string[] {
  const pieces: string[] = [];
  for (const match of line.matchAll(QUOTED)) {
    // A template's expressions are code: `${agent.harness}` names a field.
    const text = match[1] ?? match[2] ?? match[3] ?? "";
    pieces.push(text.replace(/\$\{[^}]*\}/g, "…"));
  }
  for (const match of line.matchAll(JSX_TEXT_INLINE)) {
    if (!CODE_MARKS.test(match[1])) pieces.push(match[1]);
  }
  if (isJsxTextLine(line)) pieces.push(line.trim());
  return pieces.filter(readsAsCopy);
}

/**
 * A block comment opens where code could start one; a glob such as
 * `src/**\/*.ts` inside a string does not.
 */
const COMMENT_OPEN = /(?:^|[\s{(,])\/\*/;

/** Each line with its comments removed, so prose about a rule is not copy. */
function codeLines(text: string): string[] {
  let inComment = false;
  return text.split("\n").map((raw) => {
    let line = raw;
    let kept = "";
    for (;;) {
      if (inComment) {
        const close = line.indexOf("*/");
        if (close === -1) return kept;
        line = line.slice(close + 2);
        inComment = false;
      }
      const open = COMMENT_OPEN.exec(line);
      if (!open) break;
      const at = open.index + open[0].indexOf("/*");
      kept += line.slice(0, at);
      line = line.slice(at + 2);
      inComment = true;
    }
    return (kept + line).replace(/(?:^|\s)\/\/.*$/, "");
  });
}

function offenders(
  file: string,
  text: string,
  test: (line: string) => string | null,
): string[] {
  const hits: string[] = [];
  codeLines(text).forEach((line, index) => {
    const hit = test(line);
    if (hit) hits.push(`${file}:${index + 1}  ${hit}`);
  });
  return hits;
}

/** "Work" used as a count noun anywhere a line can show text. */
function workAsCountNoun(line: string): string | null {
  return WORK_AS_COUNT_NOUN.exec(line)?.[0] ?? null;
}

/** "Harness" in a string or JSX text, never in an identifier. */
function harnessInCopy(line: string): string | null {
  return copyOn(line).find((piece) => HARNESS.test(piece)) ?? null;
}

function sweep(test: (line: string) => string | null): string[] {
  return sourceFiles().flatMap((file) =>
    offenders(file, readFileSync(join(SRC, file), "utf8"), test),
  );
}

describe("vocabulary contract (see DESIGN.md, Words)", () => {
  it("calls a conversation a conversation, never a work", () => {
    expect(
      sweep(workAsCountNoun),
      'Say "conversation" for the record: "Delete conversation", "this ' +
        'conversation\'s usage", "per conversation". "Work" names the mode ' +
        "and its navigation only.",
    ).toEqual([]);
  });

  it('says "engine" wherever people see or hear the word', () => {
    expect(
      sweep(harnessInCopy),
      'Say "engine" in labels, aria labels, placeholders, hints, and errors. ' +
        '"Harness" stays in identifiers, types, file names, routes, and wire ' +
        "values.",
    ).toEqual([]);
  });

  it("recognizes work used as a count noun", () => {
    for (const line of [
      'confirmLabel: "Delete work",',
      '<WithTooltip label="Rename work">',
      "Could not load this work",
      'toast.error(friendlyErrorMessage(err, "Could not create the work."));',
      'label="Active background agents per work"',
      'description: "Show this work\'s context and token usage.",',
      "          No turn has finished in this work yet.",
      "The work is archived.",
    ]) {
      expect(workAsCountNoun(line), line).not.toBeNull();
    }
  });

  it("leaves the mode, the verb, and the mass noun alone", () => {
    for (const line of [
      '            label: "Work",',
      "        <span>Work</span>",
      "            Work on this computer",
      '  "Tell Tidebreak how you like to work, or just keep going."',
      '    title: "Background work",',
      '"Uncommitted and unpushed work is lost and a running session is stopped."',
      "Agents can choose repositories and work across them.",
      "It stays in the Work list until you archive it.",
      "One branch per work item.",
      "const work = await inspectLiveChatWork(args);",
    ]) {
      expect(workAsCountNoun(line), line).toBeNull();
    }
  });

  it("recognizes harness in copy", () => {
    for (const line of [
      '<SelectTrigger aria-label="Harness">',
      '<SelectValue placeholder="No harness detected" />',
      '        title: "Harness payload",',
      "aria-label={`Harness: ${HARNESS_LABELS[harness]}`}",
      "            No harness detected",
      "<span>Choose a harness</span>",
      ': "Use harness default"}',
      '"The saved harness is unavailable for this channel."',
    ]) {
      expect(harnessInCopy(line), line).not.toBeNull();
    }
  });

  it("leaves identifiers, routes, and wire values alone", () => {
    for (const line of [
      "  harness: HarnessKind;",
      "    const harness = preferences?.harness;",
      "      harness,",
      '    case "harness_install":',
      '      source: "harness",',
      '  const harnessesPath: string = "/settings/coding-harnesses";',
      "  aria-label={`Stop ${HARNESS_LABELS[agent.harness]} agent`}",
      '            harness: value === "inherit" ? null : (value as HarnessKind),',
      "    harness as HarnessKind,",
      "      harness: settings.harness,",
      "    `/code/harnesses/${encodeURIComponent(kind)}/models`,",
      "  {preview.harness && `Engine: ${preview.harness}`}",
      "  if (count > harnesses.length && harnesses.length < limit) {",
    ]) {
      expect(harnessInCopy(line), line).toBeNull();
    }
  });

  it("reads past comments, including JSX comments over several lines", () => {
    const source = [
      "// Delete work used to be the label here.",
      "{/* The project's name is what truncates, so a narrow pane",
      "    still says where the work goes. */}",
      "/**",
      " * The harness doctor, shared by code mode and Settings.",
      " */",
      'const glob = "src/**/*.ts"; const label = "Delete work";',
    ].join("\n");
    expect(offenders("probe.tsx", source, workAsCountNoun)).toEqual([
      "probe.tsx:7  Delete work",
    ]);
    expect(offenders("probe.tsx", source, harnessInCopy)).toEqual([]);
  });
});
