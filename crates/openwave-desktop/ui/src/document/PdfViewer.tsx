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
import { usePdfPageState } from "@/document/usePdfPageState";
import { useWheelPageNavigation } from "@/document/useWheelPageNavigation";
import { useZoom } from "@/document/useZoom";
import { openExternal } from "@/host";
import { cn } from "@/lib/utils";

pdfjs.GlobalWorkerOptions.workerSrc = pdfWorkerUrl;

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
  /** Open on this page the first time it is requested for this document. */
  targetPage?: number;
  /** Render the toolbar at a smaller scale. */
  compact?: boolean;
}

function PdfPage({ pageNumber, width }: { pageNumber: number; width: number }) {
  return (
    <div style={{ width, display: "inline-block" }}>
      <Page
        pageNumber={pageNumber}
        width={width}
        className="shadow"
        loading={<div className="aspect-4/3 bg-background" style={{ width }} />}
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
  // an app-wide store, so only the page state needs bridging.
  const registerPdfControls = useRegisterPdfControls();
  useEffect(() => {
    if (!registerPdfControls) return;
    registerPdfControls(
      numPages > 0 ? { currentPage, numPages, setPage: setCurrentPage } : null,
    );
    return () => registerPdfControls(null);
  }, [registerPdfControls, currentPage, numPages, setCurrentPage]);

  const [containerRef, setContainerRef] = useState<HTMLElement | null>(null);
  const [containerWidth, setContainerWidth] = useState<number>(800);
  const cw = useDebouncedValue(containerWidth, 100);

  const pageWidth = (cw * scale) / 100;

  useResizeObserver(containerRef, (entry) => {
    setContainerWidth(entry.contentRect.width);
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
      {/* Scrollable page area */}
      <div className="relative min-h-0 flex-1 overflow-scroll" ref={setContainerRef}>
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
          {numPages > 0 && (
            <PdfPage
              key={`page_${currentPage}`}
              pageNumber={currentPage}
              width={pageWidth}
            />
          )}
        </Document>
      </div>
    </div>
  );
}

/** Re-rendering every page at every intermediate width while dragging a panel
 * divider is what makes resizing feel expensive; settle first, then render. */
function useDebouncedValue<T>(value: T, delay: number): T {
  const [debounced, setDebounced] = useState<T>(value);
  useEffect(() => {
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
