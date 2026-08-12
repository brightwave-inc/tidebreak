import { ChevronLeftIcon, ChevronRightIcon, MinusIcon, PlusIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { WithTooltip } from "@/components/ui/tooltip";
import type { PdfControlsData } from "@/document/PdfControlsContext";
import { useZoom } from "@/document/useZoom";

/**
 * Header variant of the PDF page picker and zoom controls. Page state comes
 * from the bridge; zoom is read from the app-wide useZoom store (the same store
 * the viewer renders from).
 */
export function PdfHeaderControls({ page }: { page: PdfControlsData }) {
  const { currentPage, numPages, setPage } = page;
  const inputValue = useZoom((s) => s.inputValue);
  const onInputChange = useZoom((s) => s.onInputChange);
  const updateScale = useZoom((s) => s.updateScale);
  const cancelInput = useZoom((s) => s.cancelInput);
  const zoomIn = useZoom((s) => s.zoomIn);
  const zoomOut = useZoom((s) => s.zoomOut);

  const goToPage = (value: string) => {
    const pageNum = parseInt(value, 10);
    if (!isNaN(pageNum) && pageNum >= 1 && pageNum <= numPages) {
      setPage(pageNum);
    }
  };

  return (
    <div className="flex items-center gap-1 text-muted-foreground">
      {numPages > 1 && (
        <>
          <WithTooltip label="Previous page">
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={() => setPage(Math.max(1, currentPage - 1))}
              disabled={currentPage <= 1}
            >
              <ChevronLeftIcon />
              <span className="sr-only">Previous page</span>
            </Button>
          </WithTooltip>
          <div className="flex items-center gap-1 text-xs">
            <input
              value={currentPage}
              onChange={(e) => goToPage(e.target.value)}
              onFocus={(e) => e.target.select()}
              aria-label="Page number"
              className="h-6 w-8 rounded border border-border bg-background text-center text-xs outline-none"
            />
            <span>/ {numPages}</span>
          </div>
          <WithTooltip label="Next page">
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={() => setPage(Math.min(numPages, currentPage + 1))}
              disabled={currentPage >= numPages}
            >
              <ChevronRightIcon />
              <span className="sr-only">Next page</span>
            </Button>
          </WithTooltip>
          <div className="mx-1 h-4 w-px bg-border" />
        </>
      )}
      <WithTooltip label="Zoom out">
        <Button variant="ghost" size="icon-sm" onClick={zoomOut}>
          <MinusIcon />
          <span className="sr-only">Zoom out</span>
        </Button>
      </WithTooltip>
      <input
        value={inputValue}
        onChange={(e) => onInputChange(e.target.value)}
        onFocus={(e) => e.target.select()}
        onBlur={updateScale}
        onKeyDown={(e) => {
          if (e.key === "Enter") updateScale();
          if (e.key === "Escape") cancelInput();
        }}
        aria-label="Zoom"
        className="h-6 w-12 rounded border border-border bg-background text-center text-xs outline-none"
      />
      <WithTooltip label="Zoom in">
        <Button variant="ghost" size="icon-sm" onClick={zoomIn}>
          <PlusIcon />
          <span className="sr-only">Zoom in</span>
        </Button>
      </WithTooltip>
    </div>
  );
}
