import { useSyncExternalStore } from "react";
import hljs from "highlight.js/lib/core";
import type { LanguageFn } from "highlight.js";

import { codeLanguageForFilename } from "@/codeLanguage";
import type { DiffFileGroup } from "../unifiedDiff";

/**
 * Syntax highlighting for diffs, from the highlighter the transcript and the
 * output viewer already ship: highlight.js.
 *
 * The note that used to sit on the diff panel gave three reasons it had no
 * highlighting. Each has an answer here:
 *
 * - Fragments misparse. A hunk is a fragment of a file, so each side of a
 *   hunk is highlighted as one continuous run, removed and unchanged lines
 *   for the old side, added and unchanged lines for the new. A comment or a
 *   string that spans lines inside the hunk parses the way it does in the
 *   file, and state resets at every hunk, so a hunk that starts inside a
 *   construct can misread no further than its own end.
 * - Colors fight the tints. The palette is six roles defined for the diff's
 *   own grounds, and `colorContrast.test.ts` holds each one to 4.5:1 on the
 *   plain row, the added and removed tints, and the word emphasis, in both
 *   themes.
 * - It costs a pass. Grammars load on first use, one per language. The view
 *   highlights a chunk of rows at a time and small hunks synchronously; the
 *   caps below leave very large files, very large hunks, and minified lines
 *   as plain text rather than stall the view.
 */

/** A diff longer than this, in lines, stays plain. */
export const MAX_HIGHLIGHT_FILE_LINES = 10_000;
/**
 * A hunk longer than this, in lines, stays plain: its removed, added, and
 * unchanged lines together, not each side on its own.
 */
export const MAX_HIGHLIGHT_HUNK_LINES = 2_500;
/** A hunk with a line longer than this, generated or minified, stays plain. */
export const MAX_HIGHLIGHT_LINE_CHARS = 1_000;

export type SyntaxRole =
  | "keyword"
  | "string"
  | "comment"
  | "number"
  | "title"
  | "attr";

export type SyntaxRun = {
  readonly text: string;
  readonly role: SyntaxRole | null;
};
export type SyntaxLine = readonly SyntaxRun[];

type GrammarModule = { default: LanguageFn };

/**
 * One loader per grammar, so Vite splits each into a chunk of its own and a
 * diff of a Rust file never downloads the TypeScript grammar.
 */
const GRAMMARS: Readonly<Record<string, () => Promise<GrammarModule>>> = {
  bash: () => import("highlight.js/lib/languages/bash"),
  c: () => import("highlight.js/lib/languages/c"),
  cpp: () => import("highlight.js/lib/languages/cpp"),
  csharp: () => import("highlight.js/lib/languages/csharp"),
  css: () => import("highlight.js/lib/languages/css"),
  go: () => import("highlight.js/lib/languages/go"),
  graphql: () => import("highlight.js/lib/languages/graphql"),
  ini: () => import("highlight.js/lib/languages/ini"),
  java: () => import("highlight.js/lib/languages/java"),
  javascript: () => import("highlight.js/lib/languages/javascript"),
  json: () => import("highlight.js/lib/languages/json"),
  kotlin: () => import("highlight.js/lib/languages/kotlin"),
  less: () => import("highlight.js/lib/languages/less"),
  lua: () => import("highlight.js/lib/languages/lua"),
  makefile: () => import("highlight.js/lib/languages/makefile"),
  markdown: () => import("highlight.js/lib/languages/markdown"),
  perl: () => import("highlight.js/lib/languages/perl"),
  php: () => import("highlight.js/lib/languages/php"),
  python: () => import("highlight.js/lib/languages/python"),
  r: () => import("highlight.js/lib/languages/r"),
  ruby: () => import("highlight.js/lib/languages/ruby"),
  rust: () => import("highlight.js/lib/languages/rust"),
  scss: () => import("highlight.js/lib/languages/scss"),
  sql: () => import("highlight.js/lib/languages/sql"),
  swift: () => import("highlight.js/lib/languages/swift"),
  typescript: () => import("highlight.js/lib/languages/typescript"),
  xml: () => import("highlight.js/lib/languages/xml"),
  yaml: () => import("highlight.js/lib/languages/yaml"),
};

/** Names the output viewer reads as something else, or not at all. */
const DIFF_EXTENSIONS: Readonly<Record<string, string>> = {
  json: "json",
  jsonc: "json",
  md: "markdown",
  mdx: "markdown",
  markdown: "markdown",
  html: "xml",
  htm: "xml",
  svg: "xml",
};

/** The grammar a diff of `path` reads with, or null for plain text. */
export function diffLanguage(path: string): string | null {
  const name = path.split(/[\\/]/).pop() ?? path;
  const extension = name.toLowerCase().split(".").pop() ?? "";
  const language =
    codeLanguageForFilename(name) ??
    (name.includes(".") ? DIFF_EXTENSIONS[extension] : undefined) ??
    null;
  return language && language in GRAMMARS ? language : null;
}

/** The diff highlighter's own instance, so its grammars never reach chat's. */
const engine = hljs.newInstance();
const ready = new Set<string>();
const pending = new Map<string, Promise<boolean>>();
const listeners = new Set<() => void>();
let readyVersion = 0;

export function isLanguageReady(language: string): boolean {
  return ready.has(language);
}

/**
 * Load one grammar. Resolves true once it can highlight, false when it
 * cannot; a failed load is left to retry on the next diff that asks.
 */
export function loadLanguage(language: string): Promise<boolean> {
  if (ready.has(language)) return Promise.resolve(true);
  const loader = GRAMMARS[language];
  if (!loader) return Promise.resolve(false);
  let promise = pending.get(language);
  if (!promise) {
    promise = loader().then(
      (module) => {
        engine.registerLanguage(language, module.default);
        ready.add(language);
        pending.delete(language);
        readyVersion += 1;
        for (const listener of listeners) listener();
        return true;
      },
      () => {
        pending.delete(language);
        return false;
      },
    );
    pending.set(language, promise);
  }
  return promise;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/**
 * Whether `language` can highlight yet, asking for its grammar the first
 * time. The view renders plain text meanwhile and colors in once it lands.
 */
export function useLanguageReady(language: string | null): boolean {
  useSyncExternalStore(
    subscribe,
    () => readyVersion,
    () => readyVersion,
  );
  if (!language) return false;
  if (!ready.has(language)) void loadLanguage(language);
  return ready.has(language);
}

/**
 * Which role a highlight.js scope plays. The diff draws six; everything
 * else inherits the role of the scope around it.
 */
const ROLE_BY_SCOPE: Readonly<Record<string, SyntaxRole>> = {
  keyword: "keyword",
  doctag: "keyword",
  "template-tag": "keyword",
  name: "keyword",
  "selector-tag": "keyword",
  string: "string",
  regexp: "string",
  "template-variable": "string",
  link: "string",
  code: "string",
  char: "string",
  comment: "comment",
  quote: "comment",
  number: "number",
  literal: "number",
  symbol: "number",
  bullet: "number",
  title: "title",
  section: "title",
  built_in: "title",
  type: "title",
  "selector-class": "title",
  "selector-id": "title",
  attr: "attr",
  attribute: "attr",
  property: "attr",
  variable: "attr",
  meta: "attr",
  "selector-attr": "attr",
  "selector-pseudo": "attr",
};

/**
 * Scopes that return to plain ink: a template's `${…}` holds code, not
 * string, however it is nested.
 */
const RESET_SCOPES = new Set(["subst"]);

function roleFor(
  className: string,
  inherited: SyntaxRole | null,
): SyntaxRole | null {
  const scope = className.split(/\s+/)[0]?.replace(/^hljs-/, "") ?? "";
  if (RESET_SCOPES.has(scope)) return null;
  const base = scope.split(".")[0] ?? scope;
  return ROLE_BY_SCOPE[scope] ?? ROLE_BY_SCOPE[base] ?? inherited;
}

const ENTITIES: Readonly<Record<string, string>> = {
  "&amp;": "&",
  "&lt;": "<",
  "&gt;": ">",
  "&quot;": '"',
  "&#x27;": "'",
  "&#39;": "'",
};

function decode(text: string): string {
  return text.includes("&")
    ? text.replace(
        /&(?:amp|lt|gt|quot|#x27|#39);/g,
        (entity) => ENTITIES[entity]!,
      )
    : text;
}

const MARKUP = /<span class="([^"]*)">|<\/span>|([^<]+)/g;

/**
 * highlight.js's markup, cut into lines of role runs.
 *
 * The markup is the engine's own escaped output: spans with class names,
 * text with five entities escaped. A span can cross a newline, a block
 * comment for one, so the open roles carry on into the next line.
 */
export function syntaxLinesFromMarkup(markup: string): SyntaxRun[][] {
  const lines: SyntaxRun[][] = [[]];
  const stack: Array<SyntaxRole | null> = [];
  for (const match of markup.matchAll(MARKUP)) {
    if (match[1] !== undefined) {
      stack.push(roleFor(match[1], stack.at(-1) ?? null));
      continue;
    }
    if (match[2] === undefined) {
      stack.pop();
      continue;
    }
    const role = stack.at(-1) ?? null;
    const parts = decode(match[2]).split("\n");
    parts.forEach((part, index) => {
      if (index > 0) lines.push([]);
      if (!part) return;
      const line = lines.at(-1)!;
      const last = line.at(-1);
      if (last && last.role === role) {
        line[line.length - 1] = { text: last.text + part, role };
      } else {
        line.push({ text: part, role });
      }
    });
  }
  return lines;
}

/**
 * Highlight `lines` as one run of `language`. Null when the grammar is not
 * loaded or the run is over a cap; the caller draws plain text then.
 */
export function highlightLines(
  lines: readonly string[],
  language: string,
): SyntaxLine[] | null {
  if (!ready.has(language)) return null;
  if (lines.length === 0) return [];
  if (lines.length > MAX_HIGHLIGHT_HUNK_LINES) return null;
  if (lines.some((line) => line.length > MAX_HIGHLIGHT_LINE_CHARS)) {
    return null;
  }
  try {
    const { value } = engine.highlight(lines.join("\n"), {
      language,
      ignoreIllegals: true,
    });
    const out = syntaxLinesFromMarkup(value);
    // The engine keeps every character, so this only guards against a
    // grammar that someday does not.
    return out.length === lines.length ? out : null;
  } catch {
    return null;
  }
}

/** One hunk's syntax: runs per line, by the line's position in the file group. */
export type HunkSyntax = {
  readonly old: ReadonlyMap<number, SyntaxLine>;
  readonly new: ReadonlyMap<number, SyntaxLine>;
};

/** Where each hunk sits among a file group's lines. */
export type HunkSpan = { readonly start: number; readonly end: number };

export function hunkSpans(group: DiffFileGroup): HunkSpan[] {
  const spans: HunkSpan[] = [];
  group.lines.forEach((line, index) => {
    if (line.kind !== "hunk") return;
    const last = spans.at(-1);
    if (last) spans[spans.length - 1] = { start: last.start, end: index };
    spans.push({ start: index, end: group.lines.length });
  });
  return spans;
}

/** Each side of a hunk: where its lines sit in the file group, and their text. */
function hunkSides(group: DiffFileGroup, span: HunkSpan) {
  const oldSources: number[] = [];
  const newSources: number[] = [];
  for (let index = span.start + 1; index < span.end; index += 1) {
    const kind = group.lines[index]!.kind;
    if (kind === "del" || kind === "context") oldSources.push(index);
    if (kind === "add" || kind === "context") newSources.push(index);
  }
  const text = (source: number) => group.lines[source]!.text.slice(1);
  return {
    oldSources,
    newSources,
    oldText: oldSources.map(text),
    newText: newSources.map(text),
  };
}

/**
 * Whether a hunk is too big to color: too many lines in all, or a line long
 * enough to be generated or minified. Checked before anything is read or
 * kept, so a hunk over a cap costs nothing past this.
 */
function overCaps(group: DiffFileGroup, span: HunkSpan): boolean {
  if (span.end - span.start - 1 > MAX_HIGHLIGHT_HUNK_LINES) return true;
  for (let index = span.start + 1; index < span.end; index += 1) {
    if (group.lines[index]!.text.length > MAX_HIGHLIGHT_LINE_CHARS + 1) {
      return true;
    }
  }
  return false;
}

type SideRuns = { old: SyntaxLine[]; new: SyntaxLine[] };

/**
 * Hunks highlighted lately, so a diff that refreshes while an agent works,
 * or a file opened again, recalls every hunk that did not change instead of
 * highlighting it again, and its colors never blink.
 *
 * Kept by a hash of the hunk's text, not the text itself, and bounded by the
 * text the entries hold rather than by their number, so a run of versions of
 * one long hunk cannot pile up. The oldest entries go first.
 */
type RecentEntry = {
  readonly runs: SideRuns;
  /** Line counts and length, checked on recall so a hash collision misses. */
  readonly shape: string;
  /** The characters the runs hold, which is what the entry costs. */
  readonly chars: number;
};

const recent = new Map<string, RecentEntry>();
let recentChars = 0;
/** The characters the recent hunks may hold together: a few megabytes. */
export const RECENT_HIGHLIGHT_CHARS = 2_000_000;

/** Characters the recent hunks hold now, for tests. */
export function recentHighlightChars(): number {
  return recentChars;
}

/**
 * Two independent 32-bit FNV-1a hashes of the hunk's language and text,
 * read in one pass. Sixty-four bits, with the shape check on top, leaves a
 * wrong recall out of reach for a cache this size.
 */
function hunkKey(language: string, sides: ReturnType<typeof hunkSides>) {
  let a = 0x811c9dc5;
  let b = 0x01000193 ^ 0x5bd1e995;
  const feed = (text: string) => {
    for (let index = 0; index < text.length; index += 1) {
      const code = text.charCodeAt(index);
      a = Math.imul(a ^ code, 0x01000193);
      b = Math.imul(b ^ code, 0x5bd1e995) ^ (b >>> 15);
    }
    // A separator no line holds, so "ab" + "c" never hashes as "a" + "bc".
    a = Math.imul(a ^ 0xffff, 0x01000193);
    b = Math.imul(b ^ 0xffff, 0x5bd1e995) ^ (b >>> 15);
  };
  feed(language);
  let chars = 0;
  for (const line of sides.oldText) {
    feed(line);
    chars += line.length;
  }
  feed("\u0001");
  for (const line of sides.newText) {
    feed(line);
    chars += line.length;
  }
  return {
    key: `${(a >>> 0).toString(36)}.${(b >>> 0).toString(36)}`,
    shape: `${sides.oldText.length}:${sides.newText.length}:${chars}`,
  };
}

function remember(key: string, entry: RecentEntry) {
  const known = recent.get(key);
  if (known) {
    recent.delete(key);
    recentChars -= known.chars;
  }
  recent.set(key, entry);
  recentChars += entry.chars;
  for (const [oldest, dropped] of recent) {
    if (recentChars <= RECENT_HIGHLIGHT_CHARS || oldest === key) break;
    recent.delete(oldest);
    recentChars -= dropped.chars;
  }
}

function runChars(lines: readonly SyntaxLine[]): number {
  let chars = 0;
  for (const line of lines) {
    for (const run of line) chars += run.text.length;
  }
  return chars;
}

function placed(
  runs: SideRuns,
  sides: ReturnType<typeof hunkSides>,
): HunkSyntax {
  return {
    old: new Map(
      sides.oldSources.map((source, index) => [source, runs.old[index]!]),
    ),
    new: new Map(
      sides.newSources.map((source, index) => [source, runs.new[index]!]),
    ),
  };
}

/**
 * Highlight one hunk, each side as its own continuous run. Null when the
 * hunk is over a cap or its grammar is missing.
 */
export function highlightHunk(
  group: DiffFileGroup,
  span: HunkSpan,
  language: string,
): HunkSyntax | null {
  if (overCaps(group, span)) return null;
  const sides = hunkSides(group, span);
  const { key, shape } = hunkKey(language, sides);
  const known = recent.get(key);
  if (known && known.shape === shape) {
    remember(key, known);
    return placed(known.runs, sides);
  }
  const oldLines = highlightLines(sides.oldText, language);
  const newLines = highlightLines(sides.newText, language);
  if (!oldLines || !newLines) return null;
  const runs = { old: oldLines, new: newLines };
  remember(key, {
    runs,
    shape,
    chars: runChars(oldLines) + runChars(newLines),
  });
  return placed(runs, sides);
}

/**
 * The hunk's syntax when an earlier diff already highlighted the same text,
 * or, for a hunk over a cap, the plain text it will always be.
 */
function recallHunk(
  group: DiffFileGroup,
  span: HunkSpan,
  language: string,
): { found: boolean; syntax: HunkSyntax | null } {
  if (overCaps(group, span)) return { found: true, syntax: null };
  const sides = hunkSides(group, span);
  const { key, shape } = hunkKey(language, sides);
  const known = recent.get(key);
  if (!known || known.shape !== shape) return { found: false, syntax: null };
  remember(key, known);
  return { found: true, syntax: placed(known.runs, sides) };
}

/**
 * The syntax of one file's diff, hunk by hunk, computed once each and kept
 * for as long as the view shows that diff. Null for a diff over the file
 * cap or in a language with no grammar.
 */
export class FileSyntax {
  readonly spans: readonly HunkSpan[];
  private readonly done = new Map<number, HunkSyntax | null>();

  constructor(
    private readonly group: DiffFileGroup,
    readonly language: string,
  ) {
    this.spans = hunkSpans(group);
  }

  static for(group: DiffFileGroup): FileSyntax | null {
    const language = diffLanguage(group.path);
    if (!language) return null;
    if (group.lines.length > MAX_HIGHLIGHT_FILE_LINES) return null;
    return new FileSyntax(group, language);
  }

  has(hunk: number): boolean {
    return this.done.has(hunk);
  }

  /** How many lines computing `hunk` would highlight. */
  size(hunk: number): number {
    const span = this.spans[hunk];
    return span ? span.end - span.start : 0;
  }

  /**
   * Take the hunk's syntax from what was highlighted lately, without
   * highlighting anything. Returns whether it is known now.
   */
  recall(hunk: number): boolean {
    if (this.done.has(hunk)) return true;
    const span = this.spans[hunk];
    if (!span) return false;
    const { found, syntax } = recallHunk(this.group, span, this.language);
    if (found) this.done.set(hunk, syntax);
    return found;
  }

  /** The hunk's syntax, computing it now if it is not known yet. */
  compute(hunk: number): HunkSyntax | null {
    if (this.done.has(hunk)) return this.done.get(hunk) ?? null;
    const span = this.spans[hunk];
    if (!span || !isLanguageReady(this.language)) return null;
    const result = highlightHunk(this.group, span, this.language);
    this.done.set(hunk, result);
    return result;
  }

  get(hunk: number): HunkSyntax | null {
    return this.done.get(hunk) ?? null;
  }
}
