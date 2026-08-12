import { ChevronRight } from "lucide-react";

import type { CitationLocator } from "@/api";

export type AssistantSource = Readonly<{
  id: string;
  ordinal: number;
  documentId: string;
  locator: CitationLocator;
}>;

type AssistantSourcesProps = {
  sources: readonly AssistantSource[];
  onOpenSource?: (source: AssistantSource) => void;
};

const MAX_SOURCES = 20;

/** The compact list of model-authored document locators below a response. */
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

  if (visibleSources.length === 0) return null;

  return (
    <details className="assistant-sources">
      <summary className="assistant-sources-toggle">
        <span className="assistant-sources-label">
          {visibleSources.length === 1
            ? "1 source"
            : `${visibleSources.length} sources`}
        </span>
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

function SourceCopy({
  source,
  onOpen,
}: {
  source: AssistantSource;
  onOpen?: (source: AssistantSource) => void;
}) {
  const body = <CitationLocatorLabel locator={source.locator} />;
  if (!onOpen) return <div className="assistant-source-copy">{body}</div>;

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

export function CitationLocatorLabel({
  locator,
}: {
  locator: CitationLocator;
}) {
  switch (locator.kind) {
    case "page":
      return <span>Page {locator.page}</span>;
    case "pages":
      return <span>Pages {locator.start}–{locator.end}</span>;
    case "lines":
      return <span>Lines {locator.start}–{locator.end}</span>;
    case "sheet":
      return (
        <span>
          Sheet {locator.sheet}
          {locator.cells ? ` · ${locator.cells}` : ""}
        </span>
      );
    case "document":
      return <span>Document</span>;
  }
}
