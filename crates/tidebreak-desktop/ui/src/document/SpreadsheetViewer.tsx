import NativeSpreadsheetViewer from "@/document/NativeSpreadsheetViewer";
import type { SheetHighlightRange } from "@/document/UniverSpreadsheetViewer";
import type { FileBytesSource } from "@/document/useFileDownload";

interface Props {
  source: FileBytesSource;
  mediaType: string;
  highlightRange?: SheetHighlightRange;
  className?: string;
}

/**
 * XLS/XLSX opens as one native, read-only workbook. The original OOXML feeds
 * the canvas engine directly, so visual fidelity and cell inspection are no
 * longer split between a PDF preview and a lossy reconstructed grid.
 */
export function SpreadsheetViewer({
  source,
  mediaType,
  highlightRange,
  className,
}: Props) {
  return (
    <NativeSpreadsheetViewer
      source={source}
      mediaType={mediaType}
      highlightRange={highlightRange}
      className={className}
    />
  );
}
