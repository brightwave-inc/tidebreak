import type { ComponentProps } from "react";

import { documentIcon } from "@/documentIcon";
import { cn } from "@/lib/utils";
import { ExcelIcon } from "./icons/excel-icon";
import { PDFIcon } from "./icons/pdf-icon";
import { PowerpointIcon } from "./icons/powerpoint-icon";
import { WordIcon } from "./icons/word-icon";

/**
 * A source's glyph, by media type.
 *
 * The four formats a reader recognises on sight — PDF, Word, Excel,
 * PowerPoint — get their own brand mark, because that is what makes a long
 * catalog scannable at a glance. Everything else falls through to the lucide
 * family glyph from {@link documentIcon}, so every media type has an icon
 * whether or not it has a brand.
 */
export function DocumentIcon({
  mediaType,
  className,
  ...props
}: ComponentProps<"svg"> & { mediaType: string | null | undefined }) {
  const type = (mediaType ?? "").split(";")[0]?.trim().toLowerCase() ?? "";
  const size = cn("size-4 shrink-0", className);

  if (type === "application/pdf") return <PDFIcon className={size} {...props} />;
  if (type.includes("wordprocessingml") || type === "application/msword") {
    return <WordIcon className={size} {...props} />;
  }
  if (
    type.includes("spreadsheetml") ||
    type === "application/vnd.ms-excel" ||
    type === "text/csv"
  ) {
    return <ExcelIcon className={size} {...props} />;
  }
  if (type.includes("presentationml") || type === "application/vnd.ms-powerpoint") {
    return <PowerpointIcon className={size} {...props} />;
  }

  const Fallback = documentIcon(type);
  return <Fallback className={size} {...props} />;
}
