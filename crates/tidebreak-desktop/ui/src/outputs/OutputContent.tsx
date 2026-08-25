/**
 * How one output revision is drawn in the detail panel.
 *
 * Formats that already have a source-document viewer reuse those engines
 * against private scratch bytes. Curated text without a dedicated engine stays
 * on the bounded preview string. Everything else offers export only.
 */
import { lazy, Suspense } from "react";

import { ImageViewer } from "@/components/document/image-viewer";
import { DocumentViewer, hasOriginalViewer } from "@/document/DocumentViewer";
import { outputFileSource } from "@/document/useFileDownload";
import { DocumentViewerState } from "@/document/ViewerPrimitives";
import {
  isTextDeliverableMediaType,
  type DeliverablePreview,
} from "@/deliverables";
import { MessageMarkdown } from "@/MessageMarkdown";
import { CodeViewer } from "./CodeViewer";

// The plotting engine is a large dependency and only chart outputs need it, so
// it is fetched from the app bundle on first use rather than at startup.
const ChartViewer = lazy(() => import("./ChartViewer"));

const CHART_MEDIA_TYPE = "application/vnd.tidebreak.chart+json";

function normalizeMediaType(mediaType: string): string {
  return mediaType.split(";", 1)[0]!.trim().toLowerCase();
}

export function OutputContent({
  chatId,
  preview,
}: {
  chatId: string;
  preview: DeliverablePreview;
}) {
  const type = normalizeMediaType(preview.mediaType);
  const source = outputFileSource(chatId, preview.outputId, preview.revisionId);

  if (hasOriginalViewer(type)) {
    return (
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
        <Suspense fallback={<ViewerLoading />}>
          <DocumentViewer
            source={source}
            mediaType={type}
            className="bg-page-background min-h-0 grow p-4 pt-2"
          />
        </Suspense>
      </div>
    );
  }

  if (type.startsWith("image/")) {
    return (
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
        <ImageViewer source={source} className="bg-page-background grow" />
      </div>
    );
  }

  if (type === CHART_MEDIA_TYPE) {
    return (
      <div className="min-h-0 flex-1 overflow-auto p-6">
        <div className="mx-auto max-w-4xl">
          <Suspense fallback={<ViewerLoading />}>
            <ChartViewer preview={preview} />
          </Suspense>
        </div>
      </div>
    );
  }

  if (!isTextDeliverableMediaType(type)) {
    return (
      <div className="min-h-0 flex-1 overflow-auto p-6">
        <p className="text-sm text-muted-foreground" role="status">
          No preview for this file type. Save as… exports the file.
        </p>
      </div>
    );
  }

  // CSV is handled above via Univer. Markdown renders as prose; other curated
  // text gets a syntax-highlighted source view.
  return (
    <div className="min-h-0 flex-1 overflow-auto p-6">
      <div className="mx-auto max-w-4xl">
        {type === "text/markdown" ? (
          <MessageMarkdown>{preview.content}</MessageMarkdown>
        ) : (
          <CodeViewer
            content={preview.content}
            mediaType={type}
            filename={preview.filename}
          />
        )}
        {preview.truncated && (
          <p className="mt-6 text-xs text-muted-foreground">
            Preview truncated. Saving writes the complete file.
          </p>
        )}
      </div>
    </div>
  );
}

function ViewerLoading() {
  return (
    <DocumentViewerState variant="loading">
      Loading preview…
    </DocumentViewerState>
  );
}
