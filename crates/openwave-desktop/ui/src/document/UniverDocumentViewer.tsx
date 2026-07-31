import { UniverDocsCorePreset } from "@univerjs/preset-docs-core";
import docsCoreEnUS from "@univerjs/preset-docs-core/locales/en-US";
import "@univerjs/preset-docs-core/lib/index.css";
import type { FUniver } from "@univerjs/presets";
import { createUniver, LocaleType } from "@univerjs/presets";
import { Loader2Icon } from "lucide-react";
import type { HTMLAttributes } from "react";
import { useEffect, useRef, useState } from "react";

import { cn } from "@/lib/utils";
import { docxToUniver } from "./docx-to-univer";
import { FileDownloadProgressIndicator } from "@/components/document/FileDownloadProgress";
import { useFileDownload, type FileBytesSource } from "./useFileDownload";

// The MODERN flavor hardcodes page width to 595/0.75 ≈ 793px. To fill the
// container we CSS-scale the render canvas so its pixel width matches the
// container width. The document component adds 20px of left page margin.
const UNIVER_MODERN_PAGE_WIDTH = 595 / 0.75;
const UNIVER_PAGE_MARGIN_LEFT = 20;
const UNIVER_EFFECTIVE_WIDTH = UNIVER_MODERN_PAGE_WIDTH + UNIVER_PAGE_MARGIN_LEFT;

// Hide the heading outline sidebar and its toggle button.
const UNIVER_VIEWER_STYLES = `
.univer-doc-viewer [id^="univer-side-menu-"],
.univer-doc-viewer .univer-min-w-\\[180px\\].univer-pt-14,
.univer-doc-viewer .univer-left-5.univer-top-4.univer-z-\\[100\\] {
    display: none !important;
}
`;

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
}

/**
 * The word-document viewer: a docx rendered in the app.
 *
 * Rendering the file rather than converting it means office formats do not
 * depend on a converter being installed on the host, which is the whole point
 * of viewing one locally.
 */
export default function UniverDocumentViewer({
  source,
  className,
  ...restProps
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const univerRef = useRef<FUniver | null>(null);
  const [errorType, setErrorType] = useState<"parse" | "load" | null>(null);
  const [isReady, setIsReady] = useState(false);
  const fileId = source.id;

  const fileDownload = useFileDownload(source, {
    parseAs: "arrayBuffer",
  });

  // Parse the docx, then mount Univer on the result.
  useEffect(() => {
    if (!fileDownload.data || !containerRef.current) return;

    if (univerRef.current) {
      univerRef.current.dispose();
      univerRef.current = null;
    }

    setIsReady(false);
    setErrorType(null);

    let cancelled = false;
    const container = containerRef.current;

    docxToUniver(fileDownload.data)
      .then((documentData) => {
        if (cancelled) return;

        const { univerAPI } = createUniver({
          locale: LocaleType.EN_US,
          locales: { [LocaleType.EN_US]: docsCoreEnUS },
          presets: [
            UniverDocsCorePreset({
              container,
              toolbar: false,
              contextMenu: false,
            }),
          ],
        });

        univerRef.current = univerAPI;
        univerAPI.createUniverDoc(documentData);

        // The MODERN flavor renders at a fixed ~793px, so apply a uniform CSS
        // scale to stretch the page to the container without distorting text.
        const containerWidth = container.clientWidth;
        if (containerWidth && containerWidth !== UNIVER_EFFECTIVE_WIDTH) {
          const scale = containerWidth / UNIVER_EFFECTIVE_WIDTH;
          const wrapper = container.firstElementChild;
          if (wrapper instanceof HTMLElement) {
            wrapper.style.transform = `scale(${scale})`;
            wrapper.style.transformOrigin = "left top";
            wrapper.style.width = `${100 / scale}%`;
            wrapper.style.height = `${100 / scale}%`;
          }
        }

        // Block editing commands while preserving scrolling and selection.
        univerAPI.addEvent(univerAPI.Event.BeforeCommandExecute, (event) => {
          if (
            event.id.startsWith("doc.command.") ||
            event.id.startsWith("doc.mutation.")
          ) {
            event.cancel = true;
          }
        });

        setIsReady(true);
      })
      .catch(() => {
        if (!cancelled) setErrorType("parse");
      });

    return () => {
      cancelled = true;
      if (univerRef.current) {
        univerRef.current.dispose();
        univerRef.current = null;
      }
    };
  }, [fileDownload.data, fileId]);

  useEffect(() => {
    if (fileDownload.error) setErrorType("load");
  }, [fileDownload.error]);

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
      className={cn("univer-doc-viewer relative flex flex-col", className)}
      {...restProps}
    >
      <style dangerouslySetInnerHTML={{ __html: UNIVER_VIEWER_STYLES }} />
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
      <div
        ref={containerRef}
        className="min-h-0 grow"
        style={{ width: "100%", height: "100%" }}
      />
    </div>
  );
}
