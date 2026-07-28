import { FileIcon, Loader2Icon } from "lucide-react";
import type { HTMLAttributes } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { cn } from "@/lib/utils";
import { extractHeadings } from "@/markdownHeadings";
import { MessageMarkdown } from "@/MessageMarkdown";
import { MarkdownOutline } from "./MarkdownOutline";
import { useFileDownload } from "./useFileDownload";

/** A character range in the raw file, as a citation reports it. */
export type HighlightRange = { start: number; end: number };

// Each chunk is rendered as a separate markdown block. 50K chars is
// comfortably fast for the parser to handle in one pass.
const CHUNK_SIZE_CHARS = 50_000;

// How many chunks to render on initial mount
const INITIAL_CHUNK_COUNT = 2;

/**
 * Find a safe content split point at or before `pos`.
 * Prefers paragraph boundaries (\n\n), falls back to any line break,
 * then to the raw position.
 */
function findSplitPoint(text: string, pos: number): number {
  const para = text.lastIndexOf("\n\n", pos);
  if (para >= 0) return para + 2;
  const line = text.lastIndexOf("\n", pos);
  if (line >= 0) return line + 1;
  return Math.max(0, pos);
}

/** Split text into chunks at paragraph/line boundaries. */
export function splitIntoChunks(text: string, chunkSize: number): string[] {
  if (text.length <= chunkSize) return [text];

  const chunks: string[] = [];
  let pos = 0;
  while (pos < text.length) {
    if (pos + chunkSize >= text.length) {
      chunks.push(text.slice(pos));
      break;
    }
    const splitPos = findSplitPoint(text, pos + chunkSize);
    // Guard against no progress (e.g. a single enormous line with no breaks)
    const end = splitPos <= pos ? pos + chunkSize : splitPos;
    chunks.push(text.slice(pos, end));
    pos = end;
  }
  return chunks;
}

/**
 * Find which chunk a character offset falls in, and the offset of that chunk's
 * start within the full text.
 */
export function findChunkForOffset(
  chunks: string[],
  offset: number,
): { index: number; chunkStart: number } | null {
  let chunkStart = 0;
  for (let i = 0; i < chunks.length; i++) {
    if (offset < chunkStart + chunks[i]!.length) {
      return { index: i, chunkStart };
    }
    chunkStart += chunks[i]!.length;
  }
  return null;
}

interface Props extends HTMLAttributes<HTMLDivElement> {
  chatId: string;
  documentID: string;
  /**
   * Character range in the raw file to reveal, as a citation reports it. The
   * range decides which chunk is rendered first and what is scrolled to;
   * drawing the highlight itself is not implemented yet.
   */
  highlightRange?: HighlightRange;
  /** Render the file as markdown rather than as the text it literally is. */
  markdown?: boolean;
}

/** Text-shaped originals: markdown rendered, everything else as written. */
export function MarkdownViewer({
  chatId,
  documentID,
  highlightRange,
  markdown = false,
  className,
  ...props
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const anchorRef = useRef<HTMLDivElement>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);

  const fileDownload = useFileDownload(chatId, documentID, { parseAs: "text" });
  const fullContent = fileDownload.data ?? "";

  // Split into render-friendly chunks at paragraph boundaries
  const chunks = useMemo(
    () => splitIntoChunks(fullContent, CHUNK_SIZE_CHARS),
    [fullContent],
  );

  const headings = useMemo(
    () => (markdown ? extractHeadings(fullContent) : []),
    [markdown, fullContent],
  );

  const scrollToHeading = useCallback((id: string) => {
    containerRef.current
      ?.querySelector(`#${CSS.escape(id)}`)
      ?.scrollIntoView({ behavior: "smooth", block: "start" });
  }, []);

  // For citation mode: locate the chunk containing the highlight
  const citationInfo = useMemo(() => {
    if (!highlightRange || !fullContent) return null;
    const hit = findChunkForOffset(chunks, highlightRange.start);
    return hit ? { chunkIndex: hit.index } : null;
  }, [highlightRange, fullContent, chunks]);

  // In citation mode, skip the chunks before it rather than parsing megabytes
  // of text the reader has not asked to see yet.
  const startChunkIndex =
    citationInfo != null ? Math.max(0, citationInfo.chunkIndex - 1) : 0;
  const maxRenderableChunks = chunks.length - startChunkIndex;
  const initialCount = Math.min(INITIAL_CHUNK_COUNT, maxRenderableChunks);

  const [renderedCount, setRenderedCount] = useState(initialCount);

  // Reset when content or citation changes
  useEffect(() => {
    setRenderedCount(initialCount);
  }, [initialCount]);

  // Load more chunks as the reader scrolls near the bottom
  useEffect(() => {
    if (renderedCount >= maxRenderableChunks) return;
    const sentinel = sentinelRef.current;
    if (!sentinel) return;

    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting) {
          setRenderedCount((prev) => Math.min(prev + 2, maxRenderableChunks));
        }
      },
      { rootMargin: "600px" },
    );
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [renderedCount, maxRenderableChunks]);

  // Scroll to the citation anchor after it renders
  useEffect(() => {
    if (!citationInfo) return;
    requestAnimationFrame(() => {
      const container = containerRef.current;
      const anchor = anchorRef.current;
      if (!container || !anchor) return;
      const containerRect = container.getBoundingClientRect();
      const anchorRect = anchor.getBoundingClientRect();
      const scrollTarget = anchorRect.top - containerRect.top + container.scrollTop;
      container.scrollTo({ top: Math.max(0, scrollTarget - 40), behavior: "instant" });
    });
  }, [citationInfo]);

  if (fileDownload.isError) {
    return (
      <div className={cn("relative overflow-auto", className)} {...props}>
        <div className="flex h-64 items-center justify-center text-muted-foreground">
          <p>Failed to download document</p>
        </div>
      </div>
    );
  }

  if (fileDownload.isLoading) {
    return (
      <div className={cn("flex items-center justify-center", className)} {...props}>
        <Loader2Icon className="size-6 animate-spin" />
      </div>
    );
  }

  // Binary files sniffed as text-adjacent can't be rendered as text
  if (fileDownload.contentType === "application/octet-stream") {
    return (
      <div className={cn("flex items-center justify-center", className)} {...props}>
        <div className="flex flex-col items-center gap-2 text-muted-foreground">
          <FileIcon className="size-8" />
          <p>No preview available for this file type</p>
        </div>
      </div>
    );
  }

  const hasMoreChunks = renderedCount < maxRenderableChunks;
  const visibleChunks = chunks.slice(startChunkIndex, startChunkIndex + renderedCount);

  return (
    <div className={cn("relative flex min-h-0 flex-1 flex-col", className)} {...props}>
      {headings.length > 0 && (
        <div className="absolute top-2 right-2 z-10">
          <MarkdownOutline
            headings={headings}
            onNavigate={scrollToHeading}
            triggerClassName="bg-background/80 backdrop-blur-sm"
          />
        </div>
      )}
      <div ref={containerRef} className="min-h-0 flex-1 overflow-auto p-6">
        <div className="mx-auto max-w-4xl">
          <div ref={anchorRef} />
          {visibleChunks.map((chunk, i) =>
            markdown ? (
              <MessageMarkdown key={startChunkIndex + i} headingIds>
                {chunk}
              </MessageMarkdown>
            ) : (
              <pre
                key={startChunkIndex + i}
                className="font-mono text-xs break-words whitespace-pre-wrap"
              >
                {chunk}
              </pre>
            ),
          )}
          {hasMoreChunks && (
            <div ref={sentinelRef} className="flex items-center justify-center py-8">
              <Loader2Icon className="size-5 animate-spin text-muted-foreground" />
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
