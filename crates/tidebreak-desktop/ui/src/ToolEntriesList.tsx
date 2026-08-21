import {
  File,
  FileOutput,
  Folder,
  Globe,
  LayoutGrid,
  Quote,
  BookOpenText as Source,
  X,
  type LucideIcon,
} from "lucide-react";

import type {
  EntriesResultPreview,
  ResultEntry,
  ResultEntryKind,
  ResultFailure,
} from "./api";
import { DocumentIcon } from "./components/document-table/DocumentIcon";
import { DomainFavicon } from "./DomainFavicon";

type ToolEntriesListProps = {
  name: string;
  /** What the call surfaced. */
  result: EntriesResultPreview;
};

/**
 * What one row is, drawn from the server's closed vocabulary.
 *
 * Keyed by [`ResultEntryKind`] so a kind added to that union without a glyph
 * fails to compile rather than rendering a hole in the middle of a list. A row
 * that names its media type outranks this map with a type-specific document
 * mark — a PDF is more recognizable as a PDF than as "a source".
 */
const ENTRY_ICONS: Record<ResultEntryKind, LucideIcon> = {
  file: File,
  folder: Folder,
  source: Source,
  passage: Quote,
  link: Globe,
  output: FileOutput,
  app: LayoutGrid,
};

/**
 * The verb for the tools whose list reads as a finding — "Searched 3 sources"
 * — drawn from the same allowlisted vocabulary as the tool's titles. A tool
 * not named here lists its rows without a count line: a single-document read
 * has nothing to count.
 */
const LIST_VERBS: Readonly<Partial<Record<string, string>>> = {
  search: "Searched",
  web_search: "Found",
  list_documents: "Checked",
  list_dir: "Found",
  list_folder: "Found",
  list_connected_folders: "Found",
};

/**
 * The list of things a call found, read, or wrote, inside the activity rail.
 *
 * A bordered card led by a muted count line, over one hoverable row per thing.
 * It renders inside
 * the expanded phase rather than as a standing card — collapsed, a phase is
 * one line of text, and what each call found is one click away.
 *
 * An empty list still renders, and that is the point: a search that matched
 * nothing and a search whose result was never projected are the same muted
 * rail line, and only one of them is a fact about the conversation.
 */
export function ToolEntriesList({ name, result }: ToolEntriesListProps) {
  const { failures, elided } = result;
  // App rows are promoted to standing cards under the phase, where the open
  // action lives. Repeated here they would be inert copies of a card the
  // reader can already act on — keyed on kind, like the promotion itself, so
  // any tool that publishes an app is covered.
  const entries = result.entries.filter((entry) => entry.kind !== "app");
  if (entries.length === 0 && failures.length === 0) {
    // A result that was all app rows has been shown in full as cards;
    // "Nothing was found" would be a different and untrue claim.
    if (result.entries.length > 0) return null;
    return (
      <div className="bg-background max-w-prose rounded-lg border p-3">
        <p className="text-muted-foreground text-xs">Nothing was found.</p>
      </div>
    );
  }
  // The count includes the rows bounded away: the line must not say the call
  // found two things when it found five and the card shows two.
  const total = entries.length + elided;
  const verb = LIST_VERBS[name];

  return (
    <div className="bg-background grid max-w-prose overflow-hidden rounded-lg border">
      {verb !== undefined && (
        <p className="text-muted-foreground border-b px-2 py-1 text-xs">
          {verb} {total} {listNoun(entries, total)}
        </p>
      )}
      {entries.length > 0 && (
        <div className="max-h-56 overflow-auto p-1">
          <ul className="flex flex-col gap-0.5">
            {entries.map((entry, index) => (
              <li key={`${entry.kind}:${entry.label}:${index}`}>
                <EntryRow entry={entry} />
              </li>
            ))}
          </ul>
        </div>
      )}
      <FailureRows failures={failures} />
      {elided > 0 && (
        <p className="text-muted-foreground border-t px-2.5 py-1.5 text-xs">
          {elided} more not shown
        </p>
      )}
    </div>
  );
}

/** The noun a kind counts as, where it is anything more specific than "item". */
const ENTRY_NOUNS: Readonly<Partial<Record<ResultEntryKind, string>>> = {
  source: "source",
  passage: "passage",
  file: "file",
  folder: "folder",
  output: "output",
  app: "app",
  link: "result",
};

/** The noun the count line counts, by what the rows uniformly are. */
function listNoun(entries: ResultEntry[], total: number): string {
  const kinds = new Set(entries.map((entry) => entry.kind));
  const kind = kinds.size === 1 ? [...kinds][0] : undefined;
  const noun = (kind === undefined ? undefined : ENTRY_NOUNS[kind]) ?? "item";
  return total === 1 ? noun : `${noun}s`;
}

function EntryRow({ entry }: { entry: ResultEntry }) {
  return (
    <div className="hover:bg-muted flex items-center gap-2 rounded p-1 px-1.5 text-sm transition-colors">
      <EntryIcon entry={entry} />
      <span className="truncate">{entry.label}</span>
      {entry.detail !== null && (
        <span className="text-muted-foreground truncate text-xs">
          {entry.detail}
        </span>
      )}
      {entry.meta !== null && (
        <span className="text-muted-foreground ml-auto shrink-0 text-xs tabular-nums">
          {entry.meta}
        </span>
      )}
    </div>
  );
}

function EntryIcon({ entry }: { entry: ResultEntry }) {
  if (entry.mediaType !== null) {
    return (
      <DocumentIcon
        mediaType={entry.mediaType}
        className="text-muted-foreground"
        aria-hidden="true"
      />
    );
  }
  // A link with an address gets its site mark; without one the kind glyph
  // stands in — same as every other row that has nowhere to fetch from.
  if (entry.kind === "link" && entry.url !== null) {
    return <DomainFavicon url={entry.url} />;
  }
  const Icon = ENTRY_ICONS[entry.kind];
  return (
    <Icon
      className="text-muted-foreground size-4 shrink-0"
      aria-hidden="true"
    />
  );
}

/**
 * What the call could not do, below what it did and divided from it.
 *
 * Its own section rather than rows mixed into the list: a failure is not a
 * result with a bad outcome, it is the absence of one, and a reader scanning
 * for what they got should not have to check each row for whether it counted.
 */
function FailureRows({ failures }: { failures: ResultFailure[] }) {
  if (failures.length === 0) return null;
  return (
    <div className="flex flex-col gap-0.5 border-t p-1">
      {failures.map((failure, index) => (
        <div
          key={`${failure.label ?? ""}:${index}`}
          className="flex items-start gap-2 rounded p-1 px-1.5 text-sm"
        >
          <X
            className="text-destructive mt-0.5 size-4 shrink-0"
            aria-hidden="true"
          />
          <span className="truncate">{failure.label ?? "Item"}</span>
          <span className="text-destructive ml-auto min-w-0 truncate text-right text-xs">
            {failure.error}
          </span>
        </div>
      ))}
    </div>
  );
}
