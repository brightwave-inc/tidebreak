import {
  ChevronLeftIcon,
  ChevronRightIcon,
  Loader2Icon,
  MinusIcon,
  PlusIcon,
} from "lucide-react";
import type { HTMLAttributes } from "react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useHotkeys } from "react-hotkeys-hook";
import { Document, Page, pdfjs } from "react-pdf";
// The worker ships as an app asset rather than being pulled from a CDN at
// runtime, so the viewer works offline and stays inside the app's content
// security policy. Vite emits the file and rewrites this to its final URL.
import pdfWorkerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";

import { FileDownloadProgressIndicator } from "@/components/document/FileDownloadProgress";
import { Button } from "@/components/ui/button";
import { useRegisterPdfControls } from "@/document/PdfControlsContext";
import {
  useFileDownload,
  type FileBytesSource,
} from "@/document/useFileDownload";
import { stabilizeMeasuredWidth } from "@/document/pdfLayout";
import { usePdfPageState } from "@/document/usePdfPageState";
import { useWheelPageNavigation } from "@/document/useWheelPageNavigation";
import { useZoom } from "@/document/useZoom";
import { openExternal } from "@/host";
import { cn } from "@/lib/utils";

pdfjs.GlobalWorkerOptions.workerSrc = pdfWorkerUrl;

/** US Letter height ÷ width — only used before the first real page paint. */
const FALLBACK_PAGE_ASPECT = 11 / 8.5;

/** Paper colour of a typical PDF page. Matches pdf.js's default canvas fill. */
const PDF_PAPER = "#ffffff";

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
  /** Open on this page the first time it is requested for this document. */
  targetPage?: number;
  /** Render the toolbar at a smaller scale. */
  compact?: boolean;
}

/**
 * One page, with the last successful paint held on top while pdf.js hides its
 * canvas to redraw. Without the holdover, every page turn, zoom, and settled
 * resize blanks the panel for a frame — and the old loading placeholder used
 * the app background colour, which in dark mode flashed dark against white
 * paper.
 */
function PdfPage({ pageNumber, width }: { pageNumber: number; width: number }) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const snapshotRef = useRef<HTMLCanvasElement>(null);
  const [snapshotSize, setSnapshotSize] = useState<{
    w: number;
    h: number;
  } | null>(null);
  // Identity of the paint currently mirrored in the snapshot canvas.
  const [snapshotKey, setSnapshotKey] = useState<string | null>(null);
  const paintKey = `${pageNumber}:${width}`;
  const holding = snapshotSize != null && snapshotKey !== paintKey;
  const snapshotHeight = snapshotSize
    ? (snapshotSize.h / snapshotSize.w) * width
    : width * FALLBACK_PAGE_ASPECT;

  const captureSnapshot = () => {
    const source = wrapRef.current?.querySelector(
      "canvas.react-pdf__Page__canvas",
    ) as HTMLCanvasElement | null;
    const target = snapshotRef.current;
    if (!source || !target || source.width === 0 || source.height === 0) {
      setSnapshotKey(paintKey);
      return;
    }
    target.width = source.width;
    target.height = source.height;
    const ctx = target.getContext("2d");
    if (!ctx) {
      setSnapshotKey(paintKey);
      return;
    }
    ctx.drawImage(source, 0, 0);
    const cssWidth = parseFloat(source.style.width) || source.width;
    const cssHeight = parseFloat(source.style.height) || source.height;
    setSnapshotSize({ w: cssWidth, h: cssHeight });
    setSnapshotKey(paintKey);
  };

  return (
    <div ref={wrapRef} className="relative inline-block" style={{ width }}>
      <canvas
        ref={snapshotRef}
        aria-hidden
        className={cn(
          "pointer-events-none absolute top-0 left-0 z-10 shadow",
          holding ? "visible" : "invisible",
        )}
        style={{ width, height: snapshotHeight }}
      />
      <Page
        pageNumber={pageNumber}
        width={width}
        className="shadow"
        canvasBackground={PDF_PAPER}
        loading={
          <div
            className={holding ? undefined : "shadow"}
            style={{
              width,
              height: snapshotHeight,
              background: holding ? "transparent" : PDF_PAPER,
            }}
          />
        }
        onRenderSuccess={captureSnapshot}
      />
    </div>
  );
}

/**
 * One page at a time, sized to the panel and scaled by the shared zoom. A
 * single-page view rather than a continuous scroll because the panel is narrow
 * and page-at-a-time is what makes wheel and arrow paging mean anything.
 */
export function PdfViewer({
  source,
  targetPage,
  compact,
  className,
  ...restProps
}: Props) {
  const scale = useZoom((s) => s.scale);
  const setScale = useZoom((s) => s.setScale);
  const updateScale = useZoom((s) => s.updateScale);
  const cancelInput = useZoom((s) => s.cancelInput);
  const inputValue = useZoom((s) => s.inputValue);
  const onInputChange = useZoom((s) => s.onInputChange);
  const zoomIn = useZoom((s) => s.zoomIn);
  const zoomOut = useZoom((s) => s.zoomOut);

  const [renderFailed, setRenderFailed] = useState(false);
  const [numPages, setNumPages] = useState(0);
  const fileId = source.id;

  const { data, error, progress } = useFileDownload(source, {
    parseAs: "arrayBuffer",
  });
  const loadFailed = renderFailed || error !== null;

  // Reset scale, error state and page count on document change.
  useEffect(() => {
    setScale(100);
    setRenderFailed(false);
    setNumPages(0);
  }, [fileId, setScale]);

  const pdfFile = useMemo(() => {
    if (!data) return null;
    // The hook hands out its own copy of the bytes, so pdf.js is free to
    // transfer this buffer without detaching the cached original.
    return { data: new Uint8Array(data) };
  }, [data]);

  const { currentPage, setCurrentPage } = usePdfPageState(fileId, {
    numPages,
    targetPage,
  });

  // Publish page state to the panel header when a provider is present. Zoom is
  // an app-wide store, so only the page state needs bridging. Register on
  // change and clear only on unmount — clearing in the update effect's cleanup
  // briefly nulls the header on every page turn.
  const registerPdfControls = useRegisterPdfControls();
  useEffect(() => {
    if (!registerPdfControls) return;
    registerPdfControls(
      numPages > 0 ? { currentPage, numPages, setPage: setCurrentPage } : null,
    );
  }, [registerPdfControls, currentPage, numPages, setCurrentPage]);
  useEffect(() => {
    if (!registerPdfControls) return;
    return () => registerPdfControls(null);
  }, [registerPdfControls]);

  const [containerRef, setContainerRef] = useState<HTMLElement | null>(null);
  const [containerWidth, setContainerWidth] = useState<number | null>(null);
  const cw = useDebouncedValue(containerWidth, 100);

  const pageWidth =
    cw != null ? Math.max(1, Math.round((cw * scale) / 100)) : null;

  useResizeObserver(containerRef, (entry) => {
    setContainerWidth((prev) =>
      stabilizeMeasuredWidth(prev, entry.contentRect.width),
    );
  });

  // pdf.js would navigate the webview itself to an external link; hand those to
  // the OS browser instead, which is the only place they can safely open.
  useEffect(() => {
    if (!containerRef) return;
    const handleClickCapture = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      const anchor = target?.closest?.("a") as HTMLAnchorElement | null;
      if (!anchor || !containerRef.contains(anchor)) return;

      const href = anchor.getAttribute("href");
      if (!href || !href.startsWith("https://")) return;

      e.preventDefault();
      e.stopPropagation();
      void openExternal(href).then((opened) => {
        if (!opened) window.open(href, "_blank", "noopener,noreferrer");
      });
    };

    containerRef.addEventListener("click", handleClickCapture, true);
    return () => containerRef.removeEventListener("click", handleClickCapture, true);
  }, [containerRef]);

  const handleKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter") updateScale();
    if (event.key === "Escape") cancelInput();
  };

  const goToPreviousPage = () => {
    if (numPages === 0) return;
    setCurrentPage((prev) => Math.max(1, prev - 1));
  };

  const goToNextPage = () => {
    if (numPages === 0) return;
    setCurrentPage((prev) => Math.min(numPages, prev + 1));
  };

  const handlePageInputChange = (value: string) => {
    const pageNum = parseInt(value, 10);
    if (!isNaN(pageNum) && pageNum >= 1 && pageNum <= numPages) {
      setCurrentPage(pageNum);
    }
  };

  useHotkeys("left", goToPreviousPage, [currentPage, numPages]);
  useHotkeys("right", goToNextPage, [currentPage, numPages]);

  useWheelPageNavigation({ containerRef, currentPage, numPages, setCurrentPage });

  if (loadFailed) {
    return (
      <div className={cn("relative overflow-scroll", className)} {...restProps}>
        <div className="flex h-64 items-center justify-center text-muted-foreground">
          <p>Failed to load document</p>
        </div>
      </div>
    );
  }

  return (
    <div
      className={cn("relative flex flex-col overflow-hidden", className)}
      {...restProps}
    >
      {/* Page and zoom controls. Hidden when the panel header hosts them. */}
      <div
        className={cn(
          "flex shrink-0 items-center justify-center gap-2",
          compact ? "pb-1" : "pb-2",
          registerPdfControls && "hidden",
        )}
      >
        {numPages > 0 && (
          <div
            className={cn(
              "flex w-auto items-center gap-1 rounded-lg bg-neutral-600 p-1 text-background",
              compact ? "h-9" : "h-11",
            )}
          >
            <Button
              variant="ghost"
              size={compact ? "icon-xs" : "icon-sm"}
              onClick={goToPreviousPage}
              disabled={currentPage === 1}
              className="hover:bg-foreground hover:text-background disabled:opacity-50"
            >
              <ChevronLeftIcon />
              <span className="sr-only">Previous page</span>
            </Button>
            <div className={cn("flex items-center gap-1", compact ? "px-1" : "px-2")}>
              <input
                min={1}
                max={numPages}
                value={currentPage}
                aria-label="Page number"
                className={cn(
                  "rounded-md bg-foreground text-center text-background outline-none",
                  compact ? "h-7 w-9 text-xs" : "h-9 w-12 text-sm",
                )}
                onChange={(e) => handlePageInputChange(e.target.value)}
                onFocus={(e) => e.target.select()}
              />
              <span
                className={cn("text-background", compact ? "text-xs" : "text-sm")}
              >
                &nbsp;&nbsp;/&nbsp;&nbsp;{numPages}
              </span>
            </div>
            <Button
              variant="ghost"
              size={compact ? "icon-xs" : "icon-sm"}
              onClick={goToNextPage}
              disabled={currentPage === numPages}
              className="hover:bg-foreground hover:text-background disabled:opacity-50"
            >
              <ChevronRightIcon />
              <span className="sr-only">Next page</span>
            </Button>
          </div>
        )}
        <div
          className={cn(
            "flex w-auto items-center gap-1 rounded-lg bg-neutral-600 p-1 text-background",
            compact ? "h-9" : "h-11",
          )}
        >
          <Button
            variant="ghost"
            size={compact ? "icon-xs" : "icon-sm"}
            onClick={zoomOut}
            className="hover:bg-foreground hover:text-background"
          >
            <MinusIcon />
            <span className="sr-only">Zoom out</span>
          </Button>
          <input
            value={inputValue}
            aria-label="Zoom"
            className={cn(
              "rounded-md bg-foreground text-center text-background outline-none",
              compact ? "h-7 w-10 text-xs" : "h-9 max-w-14 text-sm",
            )}
            onChange={(e) => onInputChange(e.target.value)}
            onFocus={(e) => e.target.select()}
            onBlur={updateScale}
            onKeyDown={handleKeyDown}
          />
          <Button
            variant="ghost"
            size={compact ? "icon-xs" : "icon-sm"}
            onClick={zoomIn}
            className="hover:bg-foreground hover:text-background"
          >
            <PlusIcon />
            <span className="sr-only">Zoom in</span>
          </Button>
        </div>
      </div>
      {/* Scrollable page area. Stable gutter keeps scrollbar appearance from
          changing the measured width and kicking off another rasterise. */}
      <div
        className="relative min-h-0 flex-1 overflow-scroll [scrollbar-gutter:stable]"
        ref={setContainerRef}
      >
        <Document
          key={fileId}
          file={pdfFile ?? undefined}
          onLoadSuccess={({ numPages }) => setNumPages(numPages)}
          onLoadError={() => setRenderFailed(true)}
          onItemClick={({ pageNumber }) => {
            if (pageNumber) {
              setCurrentPage(pageNumber);
              return false; // Suppress pdf.js's own scrolling.
            }
          }}
          noData={
            progress ? (
              <FileDownloadProgressIndicator progress={progress} />
            ) : (
              <div className="flex items-center justify-center gap-2">
                Fetching document…
                <Loader2Icon className="size-4 animate-spin" />
              </div>
            )
          }
          loading={
            <div className="flex justify-center">
              <Loader2Icon className="size-4 animate-spin" />
            </div>
          }
          error={
            <div className="flex justify-center text-muted-foreground">
              <p>Failed to load document</p>
            </div>
          }
          className="flex justify-center"
        >
          {numPages > 0 && pageWidth != null && (
            <PdfPage pageNumber={currentPage} width={pageWidth} />
          )}
        </Document>
      </div>
    </div>
  );
}

/** Re-rendering every page at every intermediate width while dragging a panel
 * divider is what makes resizing feel expensive; settle first, then render.
 * The first concrete measure paints immediately so open does not sit blank. */
function useDebouncedValue<T>(value: T, delay: number): T {
  const [debounced, setDebounced] = useState<T>(value);
  const hasConcrete = useRef(value != null);
  useEffect(() => {
    if (!hasConcrete.current && value != null) {
      hasConcrete.current = true;
      setDebounced(value);
      return;
    }
    if (!hasConcrete.current) return;
    const handle = setTimeout(() => setDebounced(value), delay);
    return () => clearTimeout(handle);
  }, [value, delay]);
  return debounced;
}

function useResizeObserver(
  element: Element | null,
  onResize: (entry: ResizeObserverEntry) => void,
): void {
  const onResizeRef = useRef(onResize);
  useEffect(() => {
    onResizeRef.current = onResize;
  });

  useEffect(() => {
    if (!element) return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry) onResizeRef.current(entry);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [element]);
}
