import { ExternalLinkIcon, FileTextIcon } from "lucide-react";

import { CitationLocatorLabel } from "@/AssistantSources";
import { DomainFavicon } from "@/DomainFavicon";
import type { OutputRevisionSource } from "@/deliverables";

export function OutputRevisionSources({
  sources,
  onOpenDocument,
  onOpenWeb,
}: {
  sources: readonly OutputRevisionSource[];
  onOpenDocument?: (
    source: Extract<OutputRevisionSource, { kind: "document" }>,
  ) => void;
  onOpenWeb?: (url: string) => void;
}) {
  if (sources.length === 0) return null;

  return (
    <section
      className="shrink-0 border-t border-border-subtle bg-page-background px-6 py-3"
      aria-label="Output sources"
    >
      <div className="mx-auto max-w-4xl">
        <h2 className="mb-2 text-xs font-medium text-muted-foreground">Sources</h2>
        <ol className="flex flex-wrap gap-1.5">
          {sources.map((source, index) => (
            <li key={sourceKey(source)}>
              {source.kind === "document" ? (
                <button
                  type="button"
                  className="inline-flex max-w-64 items-center gap-1.5 rounded-full border bg-card px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground disabled:cursor-default disabled:hover:bg-card disabled:hover:text-muted-foreground"
                  aria-label={`Open document source ${index + 1}`}
                  disabled={!onOpenDocument}
                  onClick={() => onOpenDocument?.(source)}
                >
                  <FileTextIcon className="size-3.5 shrink-0" aria-hidden="true" />
                  <span className="truncate">
                    <CitationLocatorLabel locator={source.locator} />
                  </span>
                </button>
              ) : (
                <button
                  type="button"
                  title={source.label}
                  aria-label={source.label}
                  className="inline-flex max-w-64 items-center gap-1.5 rounded-full border bg-card px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
                  onClick={() => onOpenWeb?.(source.url)}
                >
                  <DomainFavicon url={source.url} className="size-3.5" />
                  <span className="truncate">{source.domain}</span>
                  <ExternalLinkIcon className="size-3 shrink-0" aria-hidden="true" />
                </button>
              )}
            </li>
          ))}
        </ol>
      </div>
    </section>
  );
}

function sourceKey(source: OutputRevisionSource): string {
  return source.kind === "document"
    ? `document:${source.citationId}`
    : `web:${source.url}`;
}
