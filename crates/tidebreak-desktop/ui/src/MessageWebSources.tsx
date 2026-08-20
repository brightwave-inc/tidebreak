import { useState } from "react";

import type { ToolResultPreview } from "./api";
import { DomainFavicon } from "./DomainFavicon";
import { openExternal } from "./host";

/** One page a turn's web searches surfaced, as the row shows it. */
export type MessageWebSource = Readonly<{
  url: string;
  /** The page's title, or its address when the result carried no title. */
  label: string;
  /** The bare host, which is what the chip prints. */
  domain: string;
}>;

/** Chips shown before the row folds the rest into a count. */
const VISIBLE_SOURCES = 5;

/**
 * The pages behind a turn's answer, listed under it.
 *
 * A search the chat did not run itself — one the model provider performed
 * inside its own infrastructure — still has to name where its answer came
 * from, and naming it is only worth anything if the reader can reach the page.
 * So this reads from the turn's stored `web_search` rows rather than from
 * anything the model wrote in its prose: what is listed here is what was
 * actually searched.
 *
 * Deliberately domains, not titles: a row of hosts is scannable, tells the
 * reader whose account of something they are getting, and does not compete
 * with the answer above it. The title rides along as the chip's tooltip and
 * its accessible name.
 */
export function MessageWebSources({
  sources,
}: {
  sources: readonly MessageWebSource[];
}) {
  const [expanded, setExpanded] = useState(false);
  if (sources.length === 0) return null;

  const shown = expanded ? sources : sources.slice(0, VISIBLE_SOURCES);
  const hidden = sources.length - shown.length;

  return (
    <div
      className="flex flex-wrap items-center gap-1.5 px-1 pb-2"
      aria-label="Web sources"
    >
      <span className="text-xs text-muted-foreground">Sources</span>
      {shown.map((source) => (
        <button
          key={source.url}
          type="button"
          title={source.label}
          aria-label={source.label}
          className="inline-flex max-w-56 items-center gap-1 rounded-full border bg-card px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
          onClick={() => void openWebSource(source.url)}
        >
          <DomainFavicon url={source.url} className="size-3" />
          <span className="truncate">{source.domain}</span>
        </button>
      ))}
      {hidden > 0 && (
        <button
          type="button"
          className="rounded-full px-2 py-0.5 text-xs text-muted-foreground underline-offset-2 hover:underline"
          onClick={() => setExpanded(true)}
        >
          +{hidden} more
        </button>
      )}
    </div>
  );
}

/** Native opener first; `window.open` only works in a plain browser tab. */
export async function openWebSource(url: string): Promise<void> {
  if (!(await openExternal(url).catch(() => false))) {
    window.open(url, "_blank", "noreferrer,noopener");
  }
}

/**
 * The pages a turn's `web_search` calls found, in the order they were found.
 *
 * Only the search tool contributes: a page the model went on to read is part
 * of its work, not a separate claim about where the answer came from, and
 * listing it twice would say the turn consulted more than it did. Rows without
 * an address are dropped rather than shown unopenable — a source chip that
 * goes nowhere is a promise the row cannot keep. The same page found by two
 * searches is one source.
 */
export function collectWebSources(
  calls: readonly { name: string; result?: ToolResultPreview | null }[],
): MessageWebSource[] {
  const byUrl = new Map<string, MessageWebSource>();
  for (const call of calls) {
    if (call.name !== "web_search") continue;
    if (call.result?.tool !== "entries") continue;
    for (const entry of call.result.entries) {
      if (entry.kind !== "link" || !entry.url || byUrl.has(entry.url)) continue;
      byUrl.set(entry.url, {
        url: entry.url,
        label: entry.label || entry.url,
        domain: entry.detail ?? hostOf(entry.url),
      });
    }
  }
  return [...byUrl.values()];
}

/** The bare host of an address the projection carried no domain for. */
function hostOf(url: string): string {
  try {
    return new URL(url).host.replace(/^www\./, "");
  } catch {
    return url;
  }
}
