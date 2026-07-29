import { useMemo } from "react";

import type { CitationPageBounds } from "@/api";
import { useChatSessionStore } from "@/ChatSessionStore";
import type { CitationSpan } from "@/components/document/citationSpan";
import type { ChatMessage } from "@/MessageList";

/** Where in one source a citation points, as the panel needs to open it. */
export type CitationPlacement = {
  /** Half-open byte range of the passage in the document's canonical text. */
  span: CitationSpan;
  /** The page the passage was recorded on, for a paginated source. */
  page?: number;
  /**
   * Where on the pages the passage sits, for a source parsed that finely.
   * Empty for a page-granular source, which opens on its page with nothing
   * drawn on it.
   */
  bounds: readonly CitationPageBounds[];
};

/**
 * Resolve a citation against the transcript loaded beside the panel.
 *
 * Everything the panel needs is already in the open conversation — the reader
 * clicked the citation there — so this reads the session rather than asking the
 * server to look up evidence it just sent. A citation the transcript does not
 * have resolves to nothing, which is the case for a restored or shared URL
 * pointing into a conversation whose history is not on screen; the panel then
 * opens the document as though it had been reached from the source list.
 */
export function findCitationPlacement(
  messages: readonly ChatMessage[],
  documentId: string,
  citationId: string,
): CitationPlacement | null {
  for (const message of messages) {
    if (message.role !== "assistant") continue;
    for (const source of message.sources) {
      if (source.id !== citationId || source.documentId !== documentId) continue;
      return {
        span: { start: source.span.start, end: source.span.end },
        page: earliestPage(source.pages, source.bounds),
        bounds: source.bounds,
      };
    }
  }
  return null;
}

/**
 * The first page the passage was recorded on, where any was.
 *
 * A span can cross a page break, and the place to open is where it starts.
 * A page carrying a rectangle wins over one that only appears in `pages`:
 * that is the page where the reader will actually see the passage marked.
 */
function earliestPage(
  pages: readonly number[],
  bounds: readonly CitationPageBounds[],
): number | undefined {
  return lowestPage(bounds.map((rect) => rect.page)) ?? lowestPage(pages);
}

function lowestPage(pages: readonly number[]): number | undefined {
  const numbered = pages.filter((page) => Number.isSafeInteger(page) && page > 0);
  return numbered.length > 0 ? Math.min(...numbered) : undefined;
}

/** {@link findCitationPlacement} against the live session. */
export function useCitationPlacement(
  documentId: string,
  citationId: string | undefined,
): CitationPlacement | null {
  const messages = useChatSessionStore((session) => session.messages);
  return useMemo(
    () => (citationId ? findCitationPlacement(messages, documentId, citationId) : null),
    [messages, documentId, citationId],
  );
}
