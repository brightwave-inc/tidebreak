import { ChevronRight } from "lucide-react";

import type { CitationPageBounds, StructuredPathType } from "@/api";

export type AssistantSource = Readonly<{
  id: string;
  ordinal: number;
  /** The cited source, which is the document panel this row opens. */
  documentId: string;
  /**
   * Half-open byte range of the cited passage in the document's canonical
   * text — the position the source panel highlights and scrolls to.
   */
  span: Readonly<{ start: number; end: number }>;
  excerpt: string;
  heading: string | null;
  pages: number[];
  /**
   * Where on those pages the passage sits, for a source whose parser resolved
   * it that finely. Empty for page-granular sources, which is every source
   * imported before regions were recorded; `pages` is the complete answer
   * either way.
   */
  bounds: readonly CitationPageBounds[];
  /**
   * The node of a structured source the passage came from, for a source that
   * is a tree rather than a run of pages: a dot path into JSON, an XPath into
   * XML or HTML. Absent for every other kind of source, which is addressed by
   * its span and its pages.
   */
  structuredPath?: Readonly<{ path: string; pathType: StructuredPathType }>;
}>;

type AssistantSourcesProps = {
  sources: readonly AssistantSource[];
  /**
   * Open the cited place in the source panel. Omitted where there is no panel
   * to open into, which leaves the list exactly as it reads today.
   */
  onOpenSource?: (source: AssistantSource) => void;
};

// Matches the server's MAX_ASSISTANT_CITATIONS contract. Keeping the guard in
// the renderer prevents an unexpectedly large payload from growing the DOM.
const MAX_SOURCES = 20;
const MAX_HEADING_CHARACTERS = 160;
const MAX_EXCERPT_CHARACTERS = 600;
const MAX_PAGE_REFERENCES = 8;

/**
 * Evidence attached to an assistant response, as a list of places rather than a
 * list of quotations: a row opens the document it came from at the passage it
 * quoted.
 *
 * What a row carries is still closed. Storage paths, retrieval tokens, and the
 * call the evidence came from stay outside the rendered surface; the document
 * id and the cited span are here because they are the address, and neither is
 * useful without the source panel that already resolves them.
 */
export function AssistantSources({
  sources,
  onOpenSource,
}: AssistantSourcesProps) {
  const visibleSources = [...sources]
    .map((source, inputIndex) => ({ source, inputIndex }))
    .sort(
      (left, right) =>
        left.source.ordinal - right.source.ordinal ||
        left.inputIndex - right.inputIndex,
    )
    .slice(0, MAX_SOURCES);

  if (visibleSources.length === 0) {
    return null;
  }

  const sourceLabel =
    visibleSources.length === 1
      ? "1 source"
      : `${visibleSources.length} sources`;

  return (
    <details className="assistant-sources">
      <summary className="assistant-sources-toggle">
        <span className="assistant-sources-label">{sourceLabel}</span>
        <span className="assistant-source-pills" aria-hidden="true">
          {visibleSources.map(({ source, inputIndex }) => (
            <span
              className="assistant-source-pill"
              key={`${source.id}:${inputIndex}`}
            >
              {source.ordinal}
            </span>
          ))}
        </span>
        <span className="assistant-sources-chevron" aria-hidden="true">
          <ChevronRight size={15} />
        </span>
      </summary>
      <ol className="assistant-source-list">
        {visibleSources.map(({ source, inputIndex }) => (
          <li
            className="assistant-source"
            key={`${source.id}:${inputIndex}`}
            aria-label={`Source ${source.ordinal}`}
          >
            <span className="assistant-source-number" aria-hidden="true">
              {source.ordinal}
            </span>
            <SourceCopy source={source} onOpen={onOpenSource} />
          </li>
        ))}
      </ol>
    </details>
  );
}

/**
 * The row's text, as a button wherever it can be opened.
 *
 * A citation whose document id did not survive the round trip is still worth
 * reading, so it degrades to the plain excerpt rather than to a control that
 * goes nowhere.
 */
function SourceCopy({
  source,
  onOpen,
}: {
  source: AssistantSource;
  onOpen?: (source: AssistantSource) => void;
}) {
  const body = <CitationEvidence source={source} />;

  if (!onOpen || !source.documentId) {
    return <div className="assistant-source-copy">{body}</div>;
  }

  return (
    <button
      type="button"
      className="assistant-source-copy assistant-source-open"
      aria-label={`Open source ${source.ordinal}`}
      onClick={() => onOpen(source)}
    >
      {body}
    </button>
  );
}

/**
 * What one citation says for itself: where it came from and what it quoted,
 * bounded so an unexpected payload cannot grow the DOM. Shared with the popover
 * an inline citation opens, so the summary row and the phrase in the prose read
 * the same evidence the same way.
 */
export function CitationEvidence({ source }: { source: AssistantSource }) {
  const heading = boundedText(source.heading, MAX_HEADING_CHARACTERS);
  return (
    <>
      {heading ? <strong>{heading}</strong> : null}
      <p>{boundedText(source.excerpt, MAX_EXCERPT_CHARACTERS)}</p>
      <PageReferences pages={source.pages} />
    </>
  );
}

function PageReferences({ pages }: { pages: readonly number[] }) {
  const visiblePages = [...new Set(pages)]
    .filter((page) => Number.isSafeInteger(page) && page > 0)
    .sort((left, right) => left - right)
    .slice(0, MAX_PAGE_REFERENCES);

  if (visiblePages.length === 0) {
    return null;
  }

  const label = visiblePages.length === 1 ? "Page" : "Pages";
  return (
    <span className="assistant-source-pages">
      {label} {visiblePages.join(", ")}
    </span>
  );
}

function boundedText(value: string | null, maximum: number) {
  if (value === null) {
    return "";
  }

  const characters = Array.from(value.trim());
  if (characters.length <= maximum) {
    return characters.join("");
  }
  return `${characters.slice(0, maximum).join("")}…`;
}
