import {
  ChartColumn,
  File,
  FileAudio,
  FileImage,
  FileJson,
  FileSpreadsheet,
  FileText,
  FileType,
  Presentation,
  type LucideIcon,
} from "lucide-react";

/**
 * A lucide icon for a document's media-type family, shared by every list that
 * shows sources or outputs. Grouped by family rather than exact type — every
 * spreadsheet shares one glyph, every image another — and the generic fallback
 * covers anything unrecognised, so a caller always has an icon to draw.
 */
export function documentIcon(mediaType: string | null | undefined): LucideIcon {
  const type = (mediaType ?? "").split(";")[0]?.trim().toLowerCase() ?? "";

  if (type === "application/pdf") return FileText;
  if (type.includes("wordprocessingml") || type === "application/msword") {
    return FileType;
  }
  if (
    type.includes("spreadsheetml") ||
    type === "application/vnd.ms-excel" ||
    type === "text/csv"
  ) {
    return FileSpreadsheet;
  }
  if (
    type.includes("presentationml") ||
    type === "application/vnd.ms-powerpoint"
  ) {
    return Presentation;
  }
  // Charts are JSON on disk, but a reader scanning a catalog is looking for the
  // figure, not the encoding — so they get their own glyph ahead of JSON.
  if (type === "application/vnd.tidebreak.chart+json") return ChartColumn;
  if (
    type === "application/json" ||
    type.endsWith("+json") ||
    type === "application/xml" ||
    type === "text/xml" ||
    type.endsWith("+xml")
  ) {
    return FileJson;
  }
  if (type.startsWith("image/")) return FileImage;
  if (type.startsWith("audio/")) return FileAudio;
  if (type.startsWith("text/")) return FileText;
  return File;
}
