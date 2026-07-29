import { Loader2Icon } from "lucide-react";
import { lazy, Suspense } from "react";

import type { DocumentDetail } from "@/api";
import { useApp } from "@/AppContext";
import {
  DocumentViewer,
  hasOriginalViewer,
} from "@/document/DocumentViewer";
import { DocumentError } from "./error";
import { ImageViewer } from "./image-viewer";
import { MarkdownViewer, type HighlightRange } from "./markdown-viewer";

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
function structuredKind(type: string): "json" | "xml" | null {
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
  /** Character range in the original to reveal, when opened from a citation. */
  highlightRange?: HighlightRange;
  /**
   * Node to reveal in a tree viewer, when opened from a citation: a
   * dot-notation path for JSON, an XPath expression for XML.
   */
  highlightPath?: string;
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
  highlightRange,
  highlightPath,
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
              documentId={info.document_id}
              mediaType={type}
              className="bg-page-background grow p-4 pt-2"
            />
          ) : type.startsWith("image/") ? (
            <ImageViewer
              chatId={chatId}
              documentID={info.document_id}
              className="bg-page-background grow"
            />
          ) : structured !== null ? (
            <Suspense fallback={<ViewerLoading />}>
              {structured === "json" ? (
                <JsonViewer
                  chatId={chatId}
                  documentID={info.document_id}
                  highlightPath={highlightPath}
                  className="grow"
                />
              ) : (
                <XmlViewer
                  chatId={chatId}
                  documentID={info.document_id}
                  highlightPath={highlightPath}
                  className="grow"
                />
              )}
            </Suspense>
          ) : (
            <MarkdownViewer
              chatId={chatId}
              documentID={info.document_id}
              highlightRange={highlightRange}
              markdown={type === "text/markdown"}
              className="bg-page-background grow"
            />
          )}
        </>
      )}
      {view === "extracted_text" && <ExtractedText info={info} />}

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
 * That is deliberate: citation offsets index into this exact string, so the
 * day a citation has to be scrolled to and highlighted there is a single text
 * node to find the offset in.
 */
function ExtractedText({ info }: { info: DocumentDetail }) {
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
        {info.content}
      </pre>
    </div>
  );
}
