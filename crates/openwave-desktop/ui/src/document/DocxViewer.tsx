import { renderAsync } from "docx-preview";
import { Loader2Icon } from "lucide-react";
import type { HTMLAttributes } from "react";
import { useEffect, useRef, useState } from "react";

import { FileDownloadProgressIndicator } from "@/components/document/FileDownloadProgress";
import { openExternal } from "@/host";
import { cn } from "@/lib/utils";
import { useFileDownload, type FileBytesSource } from "./useFileDownload";

const PAGE_GUTTER_PX = 48;

/**
 * docx-preview intentionally renders the Word package into ordinary HTML. Its
 * embedded resources therefore stay local to the document, and disabling
 * altChunk prevents an untrusted DOCX from injecting an HTML iframe.
 */
export const DOCX_RENDER_OPTIONS = {
  breakPages: true,
  experimental: true,
  ignoreLastRenderedPageBreak: false,
  renderAltChunks: false,
  renderComments: false,
  renderEndnotes: true,
  renderFooters: true,
  renderFootnotes: true,
  renderHeaders: true,
  useBase64URL: true,
} as const;

const DOCX_VIEWER_STYLES = `
.openwave-docx-viewer .docx-wrapper {
  min-height: 100%;
  padding: 24px !important;
  background: color-mix(in srgb, var(--muted) 45%, transparent) !important;
}

.openwave-docx-viewer section.docx {
  margin: 0 auto 24px !important;
  color: #111827;
  box-shadow:
    0 1px 2px rgb(15 23 42 / 0.12),
    0 8px 28px rgb(15 23 42 / 0.10) !important;
}

.openwave-docx-viewer section.docx:last-child {
  margin-bottom: 0 !important;
}
`;

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
}

/** A read-only DOCX viewer that renders the original Word package locally. */
export default function DocxViewer({
  source,
  className,
  ...restProps
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [errorType, setErrorType] = useState<"parse" | "load" | null>(null);
  const [isReady, setIsReady] = useState(false);

  const fileDownload = useFileDownload(source, { parseAs: "arrayBuffer" });

  useEffect(() => {
    const container = containerRef.current;
    if (!fileDownload.data || !container) return;

    let cancelled = false;
    let resizeObserver: ResizeObserver | null = null;
    const staging = document.createElement("div");
    setIsReady(false);
    setErrorType(null);
    container.replaceChildren();

    // docx-preview cannot cancel a parse. Render into a detached node so a
    // superseded document can finish without overwriting the current one.
    void renderAsync(fileDownload.data, staging, staging, DOCX_RENDER_OPTIONS)
      .then(() => {
        if (cancelled) return;

        container.replaceChildren(...staging.childNodes);
        secureDocumentLinks(container);
        const pages = preparePages(container);
        const fitPages = () => fitPagesToWidth(container, pages);
        fitPages();
        resizeObserver = new ResizeObserver(fitPages);
        resizeObserver.observe(container);
        setIsReady(true);
      })
      .catch(() => {
        if (!cancelled) setErrorType("parse");
      });

    return () => {
      cancelled = true;
      resizeObserver?.disconnect();
      container.replaceChildren();
    };
  }, [fileDownload.data]);

  useEffect(() => {
    if (fileDownload.error) setErrorType("load");
  }, [fileDownload.error]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const handleClick = (event: MouseEvent) => {
      const target = event.target as HTMLElement | null;
      const anchor = target?.closest?.("a") as HTMLAnchorElement | null;
      if (!anchor || !container.contains(anchor)) return;

      const href = anchor.getAttribute("href");
      if (!href || href.startsWith("#")) return;

      event.preventDefault();
      event.stopPropagation();
      const safeHref = safeExternalHref(href);
      if (!safeHref) return;

      void openExternal(safeHref)
        .catch(() => false)
        .then((opened) => {
          if (!opened) {
            window.open(safeHref, "_blank", "noopener,noreferrer");
          }
        });
    };

    container.addEventListener("click", handleClick, true);
    return () => container.removeEventListener("click", handleClick, true);
  }, []);

  if (errorType) {
    return (
      <div className={cn("relative overflow-auto", className)} {...restProps}>
        <div className="text-muted-foreground flex h-64 items-center justify-center">
          <p>
            {errorType === "parse"
              ? "This document could not be read."
              : "This document could not be loaded."}
          </p>
        </div>
      </div>
    );
  }

  const isLoading = fileDownload.isLoading || !isReady;

  return (
    <div
      className={cn(
        "openwave-docx-viewer relative min-h-0 overflow-auto",
        className,
      )}
      {...restProps}
    >
      <style dangerouslySetInnerHTML={{ __html: DOCX_VIEWER_STYLES }} />
      {isLoading && (
        <div className="bg-background/80 absolute inset-0 z-10 flex items-center justify-center">
          {fileDownload.progress ? (
            <FileDownloadProgressIndicator progress={fileDownload.progress} />
          ) : (
            <div className="text-muted-foreground flex flex-col items-center gap-2">
              <Loader2Icon className="size-6 animate-spin" />
              <p>
                {fileDownload.isLoading
                  ? "Loading document…"
                  : "Reading document…"}
              </p>
            </div>
          )}
        </div>
      )}
      <div ref={containerRef} className="min-h-full min-w-0" />
    </div>
  );
}

type PreparedPage = { element: HTMLElement; width: number };

/**
 * Keep Word's page as the minimum height, but let content grow rather than be
 * clipped when a producer did not persist Word's last-rendered page breaks.
 */
function preparePages(container: HTMLElement): PreparedPage[] {
  return Array.from(container.querySelectorAll<HTMLElement>("section.docx")).map(
    (page) => {
      const width = page.offsetWidth;
      const height = page.style.height;
      if (height) page.style.minHeight = height;
      page.style.height = "auto";
      page.style.overflow = "visible";
      return { element: page, width };
    },
  );
}

/** Fit pages down to a narrow panel without ever enlarging or reflowing them. */
function fitPagesToWidth(container: HTMLElement, pages: PreparedPage[]): void {
  const availableWidth = Math.max(0, container.clientWidth - PAGE_GUTTER_PX);
  if (availableWidth === 0) return;
  for (const page of pages) {
    const scale = page.width > 0 ? Math.min(1, availableWidth / page.width) : 1;
    page.element.style.zoom = String(scale);
  }
}

/** Leave internal bookmarks intact; only HTTPS links may leave the document. */
function secureDocumentLinks(container: HTMLElement): void {
  for (const anchor of container.querySelectorAll<HTMLAnchorElement>("a")) {
    const href = anchor.getAttribute("href");
    if (!href || href.startsWith("#")) continue;

    const safeHref = safeExternalHref(href);

    if (safeHref === null) {
      anchor.removeAttribute("href");
      anchor.removeAttribute("target");
      anchor.removeAttribute("rel");
      continue;
    }

    anchor.href = safeHref;
    anchor.target = "_blank";
    anchor.rel = "noopener noreferrer";
  }
}

function safeExternalHref(href: string): string | null {
  try {
    const parsed = new URL(href);
    return parsed.protocol === "https:" ? parsed.href : null;
  } catch {
    // Relative links have no safe meaning outside the DOCX package.
    return null;
  }
}
