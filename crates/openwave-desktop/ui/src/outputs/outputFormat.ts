import type { DeliverableMediaType, DeliverableSummary } from "@/deliverables";

/** The reader-facing name for an output's format, and the Type facet's values. */
export function outputTypeLabel(mediaType: DeliverableMediaType): string {
  switch (mediaType) {
    case "text/markdown":
      return "Markdown";
    case "text/csv":
      return "CSV";
    case "application/json":
      return "JSON";
    case "text/html":
      return "HTML";
    case "text/plain":
      return "Plain text";
  }
}

export function revisionLabel(output: DeliverableSummary): string {
  return output.revisionCount === 1 ? "1 revision" : `${output.revisionCount} revisions`;
}
