export type AssistantSource = Readonly<{
  id: string;
  ordinal: number;
  excerpt: string;
  heading: string | null;
  pages: number[];
}>;

type AssistantSourcesProps = {
  sources: readonly AssistantSource[];
};

// Matches the server's MAX_ASSISTANT_CITATIONS contract. Keeping the guard in
// the renderer prevents an unexpectedly large payload from growing the DOM.
const MAX_SOURCES = 20;
const MAX_HEADING_CHARACTERS = 160;
const MAX_EXCERPT_CHARACTERS = 600;
const MAX_PAGE_REFERENCES = 8;

/**
 * A closed, presentation-only view of evidence attached to an assistant
 * response. It intentionally has no navigation callbacks: source identity,
 * storage paths, and retrieval tokens stay outside the rendered surface.
 */
export function AssistantSources({ sources }: AssistantSourcesProps) {
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
          ›
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
            <div className="assistant-source-copy">
              {boundedText(source.heading, MAX_HEADING_CHARACTERS) ? (
                <strong>
                  {boundedText(source.heading, MAX_HEADING_CHARACTERS)}
                </strong>
              ) : null}
              <p>{boundedText(source.excerpt, MAX_EXCERPT_CHARACTERS)}</p>
              <PageReferences pages={source.pages} />
            </div>
          </li>
        ))}
      </ol>
    </details>
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
