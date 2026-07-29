/**
 * `components/document/` is the reading side of a source: the extracted text,
 * the panel's view switcher below, and the viewers that need nothing more than
 * the file's characters — image, text/markdown, JSON, XML.
 *
 * `document/` next to it is the heavy side: the media-type dispatcher and the
 * viewers built on a document engine of their own (pdf.js, Univer), each of
 * which is code-split so opening one format does not fetch the others. The
 * download itself is shared — both sides use `@/document/useFileDownload`.
 */
import { Loader2Icon } from "lucide-react";
import { lazy, Suspense, useEffect, useMemo, useRef } from "react";

import type { CitationPageBounds, DocumentDetail } from "@/api";
import { useApp } from "@/AppContext";
import {
  DocumentViewer,
  hasOriginalViewer,
} from "@/document/DocumentViewer";
import { cn } from "@/lib/utils";
import {
  CITATION_MARK_CLASS,
  CITATION_MARK_LABEL,
  CITATION_MARK_STYLE,
} from "./citationMark";
import { charRangeForByteSpan, type CitationSpan } from "./citationSpan";
import { DocumentError } from "./error";
import { ImageViewer } from "./image-viewer";
import { MarkdownViewer } from "./markdown-viewer";

// Both tree viewers carry their own parsing and node machinery, and neither is
// on the path of the formats readers open most, so they load on demand.
const JsonViewer = lazy(() => import("./json-viewer"));
const XmlViewer = lazy(() => import("./xml-viewer"));

/** Which of a document's two views is on screen. */
export type DocumentView = "extracted_text" | "original_doc";

/**
 * A media type reduced to `type/subtype`: lowercased, parameters dropped.
 * Sources are typed by sniffing their bytes, so the stored value can carry a
 * charset that no viewer wants to match on.
 */
export function baseMediaType(mediaType: string): string {
  return mediaType.split(";")[0]?.trim().toLowerCase() ?? "";
}

/**
 * Whether any viewer can draw this format's original bytes.
 *
 * A format with no viewer is not refused: its panel drops the original view
 * and opens on the extracted text alone, which is a better floor than
 * declining to open the document at all. Keep this in step with the dispatch
 * below — a type listed here must reach a viewer there.
 */
export function isDocumentRenderable(mediaType: string): boolean {
  const type = baseMediaType(mediaType);
  return (
    type.startsWith("image/") ||
    type.startsWith("text/") ||
    structuredKind(type) !== null ||
    hasOriginalViewer(type)
  );
}

/**
 * Whether a media type is a structured tree, and which of the two it is.
 *
 * The `+json` and `+xml` suffixes are matched as well as the base types: a
 * source sniffed as `image/svg+xml` or `application/ld+json` is that tree
 * shape, and a reader opening one wants the tree rather than a wall of text.
 */
export function structuredKind(type: string): "json" | "xml" | null {
  if (type === "application/json" || type.endsWith("+json")) return "json";
  if (type === "application/xml" || type === "text/xml" || type.endsWith("+xml")) {
    return "xml";
  }
  return null;
}

type DocumentDetailsProps = {
  chatId: string;
  info: DocumentDetail;
  view: DocumentView;
  hasOriginalDocumentTab?: boolean;
  /**
   * Node to reveal in a tree viewer, when opened from a citation: a
   * dot-notation path for JSON, an XPath expression for XML.
   *
   * Recorded when the source was parsed and carried on the citation beside its
   * span, so a citation into a tree opens at the node it quoted rather than at
   * the top of the file. Absent for a citation into a source with no tree, and
   * for one made before its source's paths were recorded.
   */
  highlightPath?: string;
  /**
   * Byte range of the cited passage in the text of record, when opened from a
   * citation. Highlighted in the extracted text, and in a text-shaped original,
   * whose text of record is the file verbatim.
   */
  citationSpan?: CitationSpan;
  /** Page of a paginated original to open on, when opened from a citation. */
  targetPage?: number;
  /**
   * Where the cited passage sits on those pages, for a source parsed that
   * finely. Drawn over the rendered page; empty for a page-granular citation,
   * which opens on its page with nothing marked on it.
   */
  citationBounds?: readonly CitationPageBounds[];
};

/**
 * The two ways a source is shown, and which viewer draws each format.
 *
 * The original is whatever the reader imported, dispatched on media type. The
 * extracted text is what OpenWave actually read out of it — the text of record
 * that searches and citations index into — and every source has one, which is
 * why it is the view a format with no viewer falls back to.
 *
 * Adding a format is a branch here plus a case in {@link isDocumentRenderable}.
 */
export function DocumentDetails({
  chatId,
  info,
  view,
  hasOriginalDocumentTab,
  highlightPath,
  citationSpan,
  targetPage,
  citationBounds,
}: DocumentDetailsProps) {
  const { client } = useApp();
  const type = baseMediaType(info.media_type);
  const structured = structuredKind(type);

  return (
    <div className="flex min-h-0 grow flex-col overflow-hidden">
      {hasOriginalDocumentTab && view === "original_doc" && (
        <>
          {hasOriginalViewer(type) ? (
            <DocumentViewer
              client={client}
              chatId={chatId}
              documentId={info.document_id}
              mediaType={type}
              targetPage={targetPage}
              citationBounds={citationBounds}
              className="bg-page-background grow p-4 pt-2"
            />
          ) : type.startsWith("image/") ? (
            <ImageViewer
              client={client}
              chatId={chatId}
              documentID={info.document_id}
              className="bg-page-background grow"
            />
          ) : structured !== null ? (
            <Suspense fallback={<ViewerLoading />}>
              {structured === "json" ? (
                <JsonViewer
                  client={client}
                  chatId={chatId}
                  documentID={info.document_id}
                  highlightPath={highlightPath}
                  className="grow"
                />
              ) : (
                <XmlViewer
                  client={client}
                  chatId={chatId}
                  documentID={info.document_id}
                  highlightPath={highlightPath}
                  className="grow"
                />
              )}
            </Suspense>
          ) : (
            // Everything that reaches here is a `text/*` source, and nothing is
            // extracted out of those: the text of record is the file, so the
            // citation's own offsets address what the viewer draws.
            <MarkdownViewer
              client={client}
              chatId={chatId}
              documentID={info.document_id}
              citationSpan={citationSpan}
              markdown={type === "text/markdown"}
              className="bg-page-background grow"
            />
          )}
        </>
      )}
      {view === "extracted_text" && (
        <ExtractedText info={info} citationSpan={citationSpan} />
      )}

    </div>
  );
}

function ViewerLoading() {
  return (
    <div className="flex grow items-center justify-center bg-page-background">
      <Loader2Icon className="size-6 animate-spin text-muted-foreground" />
    </div>
  );
}

/**
 * The text of record, rendered as one contiguous run rather than paginated.
 *
 * That is what makes a citation reachable: its offsets index into this exact
 * string, so the cited passage is a slice of the rendered content rather than
 * something to go looking for. A citation splits the run in three around the
 * passage, which changes nothing about how it reads or wraps.
 */
function ExtractedText({
  info,
  citationSpan,
}: {
  info: DocumentDetail;
  citationSpan?: CitationSpan;
}) {
  // The span is derived afresh from the transcript on every update it makes, so
  // the offsets are what this depends on rather than the object holding them.
  const spanStart = citationSpan?.start;
  const spanEnd = citationSpan?.end;
  const cited = useMemo(
    () =>
      spanStart != null && spanEnd != null
        ? charRangeForByteSpan(info.content, { start: spanStart, end: spanEnd })
        : null,
    [info.content, spanStart, spanEnd],
  );

  const citedRef = useRef<HTMLElement | null>(null);
  useEffect(() => {
    if (!cited) return;
    citedRef.current?.scrollIntoView({ block: "center" });
  }, [cited]);

  if (info.content.length === 0) {
    switch (info.processing_status) {
      case "failed":
        return <DocumentError>Failed to process document</DocumentError>;
      case "ready":
        return (
          <DocumentError>
            No text could be read out of this document
          </DocumentError>
        );
      default:
        return <DocumentError>This document is still being prepared</DocumentError>;
    }
  }

  return (
    <div className="min-h-0 grow overflow-auto p-6">
      <pre className="mx-auto max-w-4xl font-sans text-sm leading-relaxed break-words whitespace-pre-wrap">
        {cited ? (
          <>
            {info.content.slice(0, cited.start)}
            <mark
              ref={citedRef}
              aria-label={CITATION_MARK_LABEL}
              className={cn(CITATION_MARK_CLASS, CITATION_MARK_STYLE)}
            >
              {info.content.slice(cited.start, cited.end)}
            </mark>
            {info.content.slice(cited.end)}
          </>
        ) : (
          info.content
        )}
      </pre>
    </div>
  );
}
