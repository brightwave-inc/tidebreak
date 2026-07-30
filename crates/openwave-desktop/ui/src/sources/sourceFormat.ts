import type { LibraryDocument } from "@/documents";
import { formatBytes } from "@/lib/formatBytes";

/**
 * How a source is named, typed, sized and dated for the catalog.
 *
 * These are the values the grid sorts, filters and facets on, so they live
 * apart from the cells that draw them — a facet count and the cell it counts
 * have to agree on what a source's type is called.
 */

export function documentTitle(document: LibraryDocument): string {
  return document.title?.trim() || `Source ${document.documentId.slice(0, 8)}`;
}

/** The reader-facing name for a media type, and the Type facet's values. */
export function mediaTypeLabel(mediaType: string): string {
  const base = mediaType.split(";")[0]?.trim().toLowerCase() ?? "";
  if (base === "application/pdf") return "PDF";
  if (base === "text/markdown") return "Markdown";
  if (base === "text/csv") return "CSV";
  if (base === "application/json" || base.endsWith("+json")) return "JSON";
  if (base === "application/xml" || base === "text/xml" || base.endsWith("+xml")) {
    return "XML";
  }
  if (base.includes("wordprocessingml") || base === "application/msword") return "Word";
  if (base.includes("spreadsheetml") || base === "application/vnd.ms-excel") return "Excel";
  if (base.includes("presentationml") || base === "application/vnd.ms-powerpoint") {
    return "PowerPoint";
  }
  if (base.startsWith("image/")) return "Image";
  if (base.startsWith("audio/")) return "Audio";
  if (base.startsWith("text/")) return "Text";
  return "File";
}

/** The Status facet's values, collapsing queued and processing into one state. */
export function statusLabel(document: LibraryDocument): string {
  switch (document.processingStatus) {
    case "queued":
    case "processing":
      return "Processing";
    case "failed":
      return "Failed";
    case "ready":
      return document.readable ? "Ready" : "No text";
  }
}

export { formatBytes as formatSize };
