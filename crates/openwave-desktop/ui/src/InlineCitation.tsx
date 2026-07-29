import { Quote } from "lucide-react";
import { createContext, useContext, type ReactNode } from "react";

import { CitationEvidence, type AssistantSource } from "./AssistantSources";
import {
  Popover,
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
 * the sentence — marked by a highlight that grows in under it on hover and a
 * small quote chip after it. It carries no ordinal: the sources row at the foot
 * of the message is where a citation is numbered, and a number in the middle of
 * a sentence only interrupts it.
 *
 * The document is the destination. A citation that names one opens it at the
 * cited passage on a single click, which is what a reader wants from an anchor
 * over a phrase; the popover is what is left when there is no document to open
 * into, and shows the evidence in place instead.
 *
 * An id that matches no snapshot is a normal state, not an error to show: the
 * phrase was authored as prose and renders as prose, without a control to press.
 * That is also why nothing here guards against a half-streamed citation — a
 * streaming message's directives are reduced to their phrasing before they reach
 * the renderer, and its snapshots arrive with the settled turn.
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

  const openDocument =
    onOpenSource && source.documentId ? () => onOpenSource(source) : undefined;

  const anchor = (
    // Named by the phrase it wraps, not by a label that would replace it: what
    // the citation backs is the sentence, and the ordinal only says which
    // citation backs it.
    <button type="button" className="inline-citation" onClick={openDocument}>
      <span className="inline-citation-phrase">{children}</span>
      <span className="sr-only">, citation {source.ordinal}</span>
      <span className="inline-citation-mark" aria-hidden="true">
        <Quote className="inline-citation-glyph" />
      </span>
    </button>
  );

  if (openDocument) return anchor;

  return (
    <Popover>
      <PopoverTrigger asChild>{anchor}</PopoverTrigger>
      <PopoverContent
        className="inline-citation-popover"
        align="start"
        side="bottom"
        collisionPadding={10}
        // Wide enough to read an excerpt as prose rather than as a column, and
        // held inside whatever the popover was given to open into.
        style={{
          width:
            "min(36rem, calc(var(--radix-popover-content-available-width) - 20px))",
          maxWidth: "calc(100vw - 20px)",
          maxHeight: "calc(var(--radix-popover-content-available-height) - 1em)",
        }}
      >
        <div className="assistant-source-copy">
          <CitationEvidence source={source} />
        </div>
      </PopoverContent>
    </Popover>
  );
}
