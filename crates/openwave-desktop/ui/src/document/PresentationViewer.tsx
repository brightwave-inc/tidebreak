/**
 * The presentation viewer: the original deck converted to PDF and drawn with
 * the PDF engine, behind a thin strip saying so.
 *
 * The conversion needs a LibreOffice the user installed. Its absence gets a
 * card with the install hint rather than an error — the deck itself is fine
 * and still exports — and a failed conversion gets its own card so a corrupt
 * file never reads as a broken app.
 */
import { Loader2Icon, PresentationIcon } from "lucide-react";
import { lazy, useMemo } from "react";

import {
  ConverterMissingError,
  presentationPdfSource,
} from "@/document/officePdf";
import {
  useFileDownload,
  type FileBytesSource,
} from "@/document/useFileDownload";
import { cn } from "@/lib/utils";

// The PDF engine is fetched on first use, same as the direct PDF branch.
const PdfViewer = lazy(() =>
  import("@/document/PdfViewer").then((m) => ({ default: m.PdfViewer })),
);

interface Props {
  /** The original presentation's bytes. */
  source: FileBytesSource;
  mediaType: string;
  className?: string;
}

export function PresentationViewer({ source, mediaType, className }: Props) {
  const pdfSource = useMemo(
    () => presentationPdfSource(source, mediaType),
    // The source object is rebuilt each render; its cache key is its identity.
    [source.cacheKey, mediaType],
  );
  const { data, isLoading, error } = useFileDownload(pdfSource, {
    parseAs: "arrayBuffer",
  });

  if (isLoading) {
    return (
      <div className="flex grow flex-col items-center justify-center gap-3">
        <Loader2Icon className="size-6 animate-spin text-muted-foreground" />
        <p className="text-sm text-muted-foreground" role="status">
          Preparing preview…
        </p>
      </div>
    );
  }

  if (error instanceof ConverterMissingError) {
    return (
      <PresentationNotice>
        Install LibreOffice to preview presentations. The file itself is fine —
        Save as… exports it unchanged.
      </PresentationNotice>
    );
  }

  if (error !== null || data === null) {
    return (
      <PresentationNotice>
        This presentation could not be converted for preview. Save as… exports
        the original file.
      </PresentationNotice>
    );
  }

  return (
    <div className="flex min-h-0 grow flex-col">
      <p className="shrink-0 px-4 pt-2 text-xs text-muted-foreground">
        Converted PDF preview — Save as… exports the original file.
      </p>
      <PdfViewer source={pdfSource} className={cn("min-h-0", className)} />
    </div>
  );
}

function PresentationNotice({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex grow items-center justify-center p-6">
      <div className="flex max-w-sm flex-col items-center gap-3 text-center">
        <PresentationIcon className="size-6 text-muted-foreground" />
        <p className="text-sm text-muted-foreground" role="status">
          {children}
        </p>
      </div>
    </div>
  );
}
