import type { DeliverableSummary } from "@/deliverables";

/** The reader-facing name for an output's format, and the Type facet's values. */
export function outputTypeLabel(mediaType: string): string {
  switch (mediaType) {
    case "text/markdown":
      return "Markdown";
    case "text/csv":
      return "CSV";
    case "application/json":
      return "JSON";
    case "application/vnd.openwave.chart+json":
      return "Chart";
    case "text/html":
      return "HTML";
    case "text/plain":
      return "Plain text";
    case "image/png":
      return "PNG image";
    case "image/jpeg":
      return "JPEG image";
    case "image/webp":
      return "WebP image";
    case "image/gif":
      return "GIF image";
    case "application/pdf":
      return "PDF";
    case "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet":
      return "Excel spreadsheet";
    case "application/vnd.openxmlformats-officedocument.wordprocessingml.document":
      return "Word document";
    case "application/vnd.openxmlformats-officedocument.presentationml.presentation":
      return "PowerPoint presentation";
    case "application/zip":
      return "ZIP archive";
    default:
      return "File";
  }
}

export function revisionLabel(output: DeliverableSummary): string {
  return output.revisionCount === 1 ? "1 revision" : `${output.revisionCount} revisions`;
}
