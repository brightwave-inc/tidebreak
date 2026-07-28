import { lazy, Suspense } from "react";
import { Loader2Icon } from "lucide-react";

import type { ApiClient } from "@/api";

// pdf.js is a large dependency and most sessions never open a PDF, so it is
// fetched from the app bundle on first use rather than at startup.
const PdfViewer = lazy(() =>
  import("@/document/PdfViewer").then((m) => ({ default: m.PdfViewer })),
);

/**
 * Which viewer renders a source in its original form, chosen by media type.
 *
 * The dispatcher is the registration point for every format we learn to show:
 * a new viewer is one branch here plus one entry in {@link hasOriginalViewer}.
 * Formats with no viewer return null and the document panel falls back to the
 * extracted text, which is a better floor than refusing to open the source.
 */
export function hasOriginalViewer(mediaType: string): boolean {
  return normalizeMediaType(mediaType) === "application/pdf";
}

interface DocumentViewerProps {
  client: Pick<ApiClient, "getDocumentFileContent">;
  documentId: string;
  mediaType: string;
  /** Open on this page the first time it is requested for this document. */
  targetPage?: number;
  className?: string;
}

export function DocumentViewer({
  client,
  documentId,
  mediaType,
  targetPage,
  className,
}: DocumentViewerProps) {
  switch (normalizeMediaType(mediaType)) {
    case "application/pdf":
      return (
        <Suspense
          fallback={
            <div className="flex grow items-center justify-center">
              <Loader2Icon className="size-6 animate-spin text-muted-foreground" />
            </div>
          }
        >
          <PdfViewer
            client={client}
            documentId={documentId}
            targetPage={targetPage}
            className={className}
          />
        </Suspense>
      );
    default:
      return null;
  }
}

/** Media types arrive with parameters (`application/pdf; charset=binary`). */
function normalizeMediaType(mediaType: string): string {
  return mediaType.split(";", 1)[0]!.trim().toLowerCase();
}
