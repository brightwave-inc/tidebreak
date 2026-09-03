import type { ComponentType } from "react";

import { fuzzyScore } from "./fuzzy";
import type { ShellShortcutAction } from "./ShellShortcuts";
import type { StatusTone } from "./code/statusTone";

/**
 * The command palette's data model, kept apart from the dialog that draws it.
 *
 * Everything here is pure: rows in, ordered sections out. The palette is the
 * one surface that reaches every other one, so the temptation is to let it
 * import every store and decide what it can see. Instead each half of the app
 * hands it rows it already knows how to build, and this file only decides what
 * the reader is looking for and which rows answer it.
 *
 * That split is what makes the ordering testable without a DOM, and what keeps
 * a new command a change to one source rather than to the palette.
 */

/**
 * A band of related rows, drawn under its own heading.
 *
 * A section is a kind of answer, not a place the row came from: workspace
 * commands and repo quick actions are both "Actions" because a reader looking
 * for something to do does not care which list defined it.
 */
export type PaletteSection =
  | "suggested"
  | "workspaces"
  | "chats"
  | "actions"
  | "ship"
  | "files"
  | "settings"
  | "navigate";

/** The order sections appear in. Empty ones are dropped rather than drawn. */
export const PALETTE_SECTION_ORDER: readonly PaletteSection[] = [
  "suggested",
  "workspaces",
  "chats",
  "actions",
  "ship",
  "files",
  "settings",
  "navigate",
];

export const PALETTE_SECTION_LABELS: Record<PaletteSection, string> = {
  suggested: "Suggested",
  workspaces: "Workspaces",
  chats: "Recent work",
  actions: "Actions",
  ship: "Ship",
  files: "Files",
  settings: "Settings",
  navigate: "Go to",
};

/** A lucide glyph, typed the way the settings table already types its icons. */
export type PaletteIcon = ComponentType<{ size?: number; className?: string }>;

export type PaletteRow = {
  /**
   * Stable across renders and queries — this is what the recents list
   * remembers, so a generated id would quietly disable the memory.
   */
  id: string;
  section: PaletteSection;
  label: string;
  /**
   * Words the query may match that the row does not print: a branch name, a
   * repo, the old name of a renamed command. Matching on more than the label
   * is what lets `wsp` find a workspace whose title never says "workspace".
   */
  keywords?: string;
  /** Muted trailing text — a repo, a parent path, a relative age. */
  hint?: string;
  /** The accent this row's state carries, matching the rail's own tones. */
  tone?: StatusTone;
  icon?: PaletteIcon;
  /**
   * The chord that does this without the palette, drawn from the shortcut
   * table. A palette that teaches its own shortcuts is how a reader stops
   * needing it.
   */
  shortcut?: ShellShortcutAction;
  /** What picking the row does. */
  onSelect: () => void;
  /** Kept out of the recents memory — a row nobody means to repeat. */
  transient?: boolean;
};

/**
 * The typed prefixes that narrow the list to one kind of answer.
 *
 * Sections already tell the reader what kinds exist, so these are an
 * accelerator rather than the only way through — which is why they are listed
 * in the footer instead of being drawn as tabs. Tabs would say the same thing
 * twice and only the mouse could press them.
 */
export const PALETTE_PREFIXES: readonly {
  char: string;
  sections: readonly PaletteSection[];
  label: string;
}[] = [
  { char: ">", sections: ["actions", "ship"], label: "commands" },
  { char: "@", sections: ["workspaces", "chats"], label: "go to" },
  { char: "#", sections: ["files"], label: "files" },
];

export type ParsedPaletteQuery = {
  /** The sections a prefix limits the list to, or `null` for all of them. */
  sections: readonly PaletteSection[] | null;
  /** The prefix that did the limiting, for the scope chip. */
  prefix: string | null;
  scopeLabel: string | null;
  /** What rows are actually matched against, with the prefix stripped. */
  query: string;
};

/**
 * Split a raw input into the scope it asks for and the words to match.
 *
 * Only a leading prefix counts. A `#` in the middle of a query is part of what
 * the reader is searching for — issue numbers and CSS colors both have one —
 * and treating it as a scope would make the list jump as they typed.
 */
export function parsePaletteQuery(raw: string): ParsedPaletteQuery {
  const trimmed = raw.trimStart();
  for (const prefix of PALETTE_PREFIXES) {
    if (!trimmed.startsWith(prefix.char)) continue;
    return {
      sections: prefix.sections,
      prefix: prefix.char,
      scopeLabel: prefix.label,
      query: trimmed.slice(prefix.char.length).trim(),
    };
  }
  return { sections: null, prefix: null, scopeLabel: null, query: raw.trim() };
}

/** How many rows a section shows before the reader has to narrow. */
const SECTION_CAP = 5;
/** The cap once a prefix says this is the only kind being looked at. */
const SCOPED_CAP = 12;
/** The suggestion is one row by definition; two would not be a suggestion. */
const SUGGESTED_CAP = 1;

export type PaletteGroup = {
  section: PaletteSection;
  label: string;
  rows: PaletteRow[];
};

/**
 * The rows that answer a query, grouped and capped.
 *
 * Scoring is per section rather than across the whole list, because sections
 * are ordered by what the reader most likely wants and a strong text match on
 * a settings page should not push the workspace they are looking at below it.
 * Recents break ties: with nothing typed every score is zero, so the memory is
 * the only thing ordering rows, and the palette opens on what was used last.
 */
export function rankPaletteRows(
  rows: readonly PaletteRow[],
  raw: string,
  options: { recents?: readonly string[] } = {},
): PaletteGroup[] {
  const parsed = parsePaletteQuery(raw);
  const recents = options.recents ?? [];
  const scored = new Map<PaletteSection, { row: PaletteRow; rank: number }[]>();

  for (const row of rows) {
    if (parsed.sections && !parsed.sections.includes(row.section)) continue;
    const haystack = row.keywords ? `${row.label} ${row.keywords}` : row.label;
    const score = fuzzyScore(haystack, parsed.query);
    if (score < 0) continue;
    const bucket = scored.get(row.section) ?? [];
    bucket.push({ row, rank: score + recentBonus(row.id, recents) });
    scored.set(row.section, bucket);
  }

  const cap = parsed.sections ? SCOPED_CAP : SECTION_CAP;
  return PALETTE_SECTION_ORDER.flatMap((section) => {
    const bucket = scored.get(section);
    if (!bucket || bucket.length === 0) return [];
    // A stable sort keeps the source's own order — workspaces by recency,
    // settings in rail order — wherever the ranks come out equal.
    const ordered = bucket
      .slice()
      .sort((left, right) => right.rank - left.rank)
      .slice(0, section === "suggested" ? SUGGESTED_CAP : cap)
      .map((entry) => entry.row);
    return [
      {
        section,
        label: PALETTE_SECTION_LABELS[section],
        rows: ordered,
      },
    ];
  });
}

/**
 * How much a recently picked row floats.
 *
 * Small enough that a real text match still wins — a reader who types is
 * describing what they want, not asking for their history — and large enough
 * to decide an otherwise flat list.
 */
function recentBonus(id: string, recents: readonly string[]): number {
  const index = recents.indexOf(id);
  return index < 0 ? 0 : (recents.length - index) * 40;
}

const RECENTS_KEY = "tidebreak.command-palette-recents";
/** Long enough to cover a working session's habits, short enough to turn over. */
const RECENTS_LIMIT = 24;

export function readPaletteRecents(): string[] {
  try {
    const raw = window.localStorage.getItem(RECENTS_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((id): id is string => typeof id === "string");
  } catch {
    return [];
  }
}

/** Move `id` to the front of the memory and return the new list. */
export function rememberPaletteRow(
  id: string,
  recents: readonly string[],
): string[] {
  const next = [id, ...recents.filter((entry) => entry !== id)].slice(
    0,
    RECENTS_LIMIT,
  );
  try {
    window.localStorage.setItem(RECENTS_KEY, JSON.stringify(next));
  } catch {
    // Preference persistence is best-effort.
  }
  return next;
}
