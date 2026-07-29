import { createContext, useContext, type ReactNode } from "react";

import { CitationEvidence, type AssistantSource } from "./AssistantSources";
import {
  Popover,
  PopoverClose,
  PopoverContent,
  PopoverTrigger,
} from "./components/ui/popover";

/**
 * The citations one message carries, and how to open one.
 *
 * A cited phrase is rendered several components below the message that owns the
 * evidence — inside the Markdown pipeline, which is memoized per block so that a
 * streaming message re-parses only its tail. Threading the snapshots through
 * that as props would change the identity of every block on every token and
 * defeat exactly the memoization it exists for, so they are read where the
 * citation is instead. Outside a provider a citation reads as plain prose.
 */
export type MessageCitations = {
  sources: readonly AssistantSource[];
  /** Open the cited place in the source panel — the sources row's own path. */
  onOpenSource?: (source: AssistantSource) => void;
};

const MessageCitationsContext = createContext<MessageCitations>({
  sources: [],
});

export const MessageCitationsProvider = MessageCitationsContext.Provider;

export function useMessageCitations(): MessageCitations {
  return useContext(MessageCitationsContext);
}

const CITATION_ID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/**
 * A cited phrase, in the prose where the model wrote it.
 *
 * The phrase stays prose — it inherits the surrounding type and reads as part of
 * the sentence — with the citation's ordinal beside it and its evidence a click
 * away. The ordinal comes from the snapshot rather than from the order spans
 * happen to appear in: a citation dropped past the message's bound leaves a gap,
 * and a badge counted off the rendering would silently renumber the rest.
 *
 * An id that matches no snapshot is a normal state, not an error to show: the
 * phrase was authored as prose and renders as prose, without a badge to press.
 */
export function InlineCitation({
  citationId,
  children,
}: {
  citationId: string;
  children?: ReactNode;
}) {
  const { sources, onOpenSource } = useMessageCitations();
  const source = CITATION_ID.test(citationId)
    ? sources.find(
        (candidate) =>
          candidate.id.toLowerCase() === citationId.toLowerCase(),
      )
    : undefined;

  if (!source) return <>{children}</>;

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className="inline-citation"
          aria-label={`Citation ${source.ordinal}`}
        >
          {children}
          <sup className="inline-citation-ordinal" aria-hidden="true">
            {source.ordinal}
          </sup>
        </button>
      </PopoverTrigger>
      <PopoverContent
        className="inline-citation-popover w-80"
        align="start"
        side="bottom"
      >
        <div className="assistant-source-copy">
          <CitationEvidence source={source} />
        </div>
        {onOpenSource && source.documentId ? (
          <PopoverClose asChild>
            <button
              type="button"
              className="inline-citation-open"
              onClick={() => onOpenSource(source)}
            >
              Open source
            </button>
          </PopoverClose>
        ) : null}
      </PopoverContent>
    </Popover>
  );
}
