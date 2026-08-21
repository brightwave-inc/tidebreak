import { FileIcon, Loader2Icon } from "lucide-react";
import type { HTMLAttributes } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  useFileDownload,
  type FileBytesSource,
} from "@/document/useFileDownload";
import { cn } from "@/lib/utils";
import { extractHeadings } from "@/markdownHeadings";
import { MessageMarkdown } from "@/MessageMarkdown";
import {
  CITATION_MARK_CLASS,
  CITATION_MARK_LABEL,
  CITATION_MARK_STYLE,
  pieceStartOffsets,
  rangeWithinPiece,
} from "./citationMark";
import { FileDownloadProgressIndicator } from "./FileDownloadProgress";
import { MarkdownOutline } from "./MarkdownOutline";

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

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
  /** Model-authored line range to reveal when a citation led here. */
  targetLines?: Readonly<{ start: number; end: number }>;
  /** Render the file as markdown rather than as the text it literally is. */
  markdown?: boolean;
}

/** Text-shaped originals: markdown rendered, everything else as written. */
export function MarkdownViewer({
  source,
  targetLines,
  markdown = false,
  className,
  ...props
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);

  const fileDownload = useFileDownload(source, {
    parseAs: "text",
  });
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

  const cited = useMemo(
    () =>
      targetLines && fullContent
        ? characterRangeForLines(fullContent, targetLines)
        : null,
    [fullContent, targetLines],
  );

  const chunkStarts = useMemo(() => pieceStartOffsets(chunks), [chunks]);

  // For citation mode: the chunk the passage begins in — the last one starting
  // at or before it.
  const citationChunk = useMemo(() => {
    if (!cited) return null;
    for (let i = chunkStarts.length - 1; i >= 0; i--) {
      if (chunkStarts[i]! <= cited.start) return i;
    }
    return null;
  }, [cited, chunkStarts]);

  // In citation mode, skip the chunks before it rather than parsing megabytes
  // of text the reader has not asked to see yet.
  const startChunkIndex =
    citationChunk != null ? Math.max(0, citationChunk - 1) : 0;
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

  // Scroll to the passage itself, whichever way the original is drawn: the mark
  // is what the reader came for, and a range marked in several nodes — a
  // sentence with a bold word in it — begins at the first of them.
  //
  // Once per passage, not once per render. The mark can arrive a commit after
  // the citation resolves, since the chunk holding it may not be among those
  // already drawn, and appending chunks as the reader scrolls must not drag them
  // back to where they started reading.
  const scrolledTo = useRef<string | null>(null);
  useEffect(() => {
    if (!cited) return;
    const passage = `${cited.start}:${cited.end}`;
    if (scrolledTo.current === passage) return;
    const mark = containerRef.current?.querySelector(`.${CITATION_MARK_CLASS}`);
    if (!mark) return;
    scrolledTo.current = passage;
    mark.scrollIntoView({ block: "center" });
  }, [cited, renderedCount]);

  if (fileDownload.error) {
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
      <div
        className={cn("flex items-center justify-center", className)}
        {...props}
      >
        {fileDownload.progress ? (
          <FileDownloadProgressIndicator progress={fileDownload.progress} />
        ) : (
          <Loader2Icon className="size-6 animate-spin" />
        )}
      </div>
    );
  }

  // Binary files sniffed as text-adjacent can't be rendered as text
  if (fileDownload.contentType === "application/octet-stream") {
    return (
      <div
        className={cn("flex items-center justify-center", className)}
        {...props}
      >
        <div className="flex flex-col items-center gap-2 text-muted-foreground">
          <FileIcon className="size-8" />
          <p>No preview available for this file type</p>
        </div>
      </div>
    );
  }

  const hasMoreChunks = renderedCount < maxRenderableChunks;
  const visibleChunks = chunks.slice(
    startChunkIndex,
    startChunkIndex + renderedCount,
  );

  return (
    <div
      className={cn("relative flex min-h-0 flex-1 flex-col", className)}
      {...props}
    >
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
          {visibleChunks.map((chunk, i) => {
            const inChunk = cited
              ? rangeWithinPiece(
                  cited,
                  chunkStarts[startChunkIndex + i]!,
                  chunk.length,
                )
              : null;
            return markdown ? (
              <MessageMarkdown
                key={startChunkIndex + i}
                headingIds
                highlightRange={inChunk ?? undefined}
              >
                {chunk}
              </MessageMarkdown>
            ) : (
              <pre
                key={startChunkIndex + i}
                className="font-mono text-xs break-words whitespace-pre-wrap"
              >
                {inChunk ? (
                  <>
                    {chunk.slice(0, inChunk.start)}
                    <mark
                      aria-label={CITATION_MARK_LABEL}
                      className={cn(CITATION_MARK_CLASS, CITATION_MARK_STYLE)}
                    >
                      {chunk.slice(inChunk.start, inChunk.end)}
                    </mark>
                    {chunk.slice(inChunk.end)}
                  </>
                ) : (
                  chunk
                )}
              </pre>
            );
          })}
          {hasMoreChunks && (
            <div
              ref={sentinelRef}
              className="flex items-center justify-center py-8"
            >
              <Loader2Icon className="size-5 animate-spin text-muted-foreground" />
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function characterRangeForLines(
  content: string,
  lines: Readonly<{ start: number; end: number }>,
) {
  const starts = [0];
  for (let index = 0; index < content.length; index += 1) {
    if (content[index] === "\n") starts.push(index + 1);
  }
  if (lines.start > starts.length) return null;
  const start = starts[Math.max(0, lines.start - 1)] ?? 0;
  const end = starts[Math.min(lines.end, starts.length)] ?? content.length;
  return { start, end };
}
