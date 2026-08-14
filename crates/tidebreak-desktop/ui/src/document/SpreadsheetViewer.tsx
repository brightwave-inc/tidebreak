import { EyeIcon, TablePropertiesIcon } from "lucide-react";
import { useEffect, useState } from "react";

import { Button } from "@/components/ui/button";
import { ConvertedOfficeViewer } from "@/document/PresentationViewer";
import UniverSpreadsheetViewer, {
  type SheetHighlightRange,
} from "@/document/UniverSpreadsheetViewer";
import type { FileBytesSource } from "@/document/useFileDownload";
import { cn } from "@/lib/utils";

interface Props {
  source: FileBytesSource;
  mediaType: string;
  highlightRange?: SheetHighlightRange;
  className?: string;
}

type SpreadsheetView = "preview" | "inspect";

/**
 * Two deliberately different readings of a workbook:
 *
 * - Preview renders LibreOffice's complete-sheet PDF, preserving the authored
 *   visual document including charts and drawings.
 * - Inspect keeps the interactive Univer grid for formulas, selection,
 *   keyboard navigation, and cell-range citations.
 */
export function SpreadsheetViewer({
  source,
  mediaType,
  highlightRange,
  className,
}: Props) {
  const [view, setView] = useState<SpreadsheetView>(() =>
    highlightRange?.startCell ? "inspect" : "preview",
  );

  // A citation names cells, not a position on a rendered PDF. Move to the
  // representation that can select and center that exact range.
  useEffect(() => {
    if (highlightRange?.startCell) setView("inspect");
  }, [highlightRange]);

  return (
    <div className={cn("flex min-h-0 flex-col", className)}>
      <div className="flex h-10 shrink-0 items-center gap-1 border-b px-3">
        <Button
          variant={view === "preview" ? "secondary" : "ghost"}
          size="sm"
          aria-pressed={view === "preview"}
          onClick={() => setView("preview")}
        >
          <EyeIcon />
          Preview
        </Button>
        <Button
          variant={view === "inspect" ? "secondary" : "ghost"}
          size="sm"
          aria-pressed={view === "inspect"}
          onClick={() => setView("inspect")}
        >
          <TablePropertiesIcon />
          Inspect cells
        </Button>
        <span className="ml-2 text-xs text-muted-foreground">
          {view === "preview"
            ? "Rendered for visual fidelity"
            : "Read-only formulas and cell navigation"}
        </span>
      </div>

      <div className="min-h-0 grow">
        {view === "preview" ? (
          <ConvertedOfficeViewer
            source={source}
            mediaType={mediaType}
            kind="spreadsheet"
            className="h-full"
          />
        ) : (
          <UniverSpreadsheetViewer
            key={source.id}
            source={source}
            highlightRange={highlightRange}
            className="h-full"
          />
        )}
      </div>
    </div>
  );
}
