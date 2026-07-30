import { Quote } from "lucide-react";
import { createContext, useContext, type ReactNode } from "react";

import type { CitationLocator } from "./api";
import type { AssistantSource } from "./AssistantSources";

export type MessageCitations = {
  sources: readonly AssistantSource[];
  onOpenSource?: (source: AssistantSource) => void;
};

const MessageCitationsContext = createContext<MessageCitations>({
  sources: [],
});

export const MessageCitationsProvider = MessageCitationsContext.Provider;

export function useMessageCitations(): MessageCitations {
  return useContext(MessageCitationsContext);
}

/** A cited phrase and the small locator chip that opens its document. */
export function InlineCitation({
  documentId,
  locator,
  children,
}: {
  documentId: string;
  locator: CitationLocator;
  children?: ReactNode;
}) {
  const { sources, onOpenSource } = useMessageCitations();
  const serializedLocator = JSON.stringify(locator);
  const source = sources.find(
    (candidate) =>
      candidate.documentId.toLowerCase() === documentId.toLowerCase() &&
      JSON.stringify(candidate.locator) === serializedLocator,
  );

  if (!source || !onOpenSource) return <>{children}</>;

  return (
    <button
      type="button"
      className="inline-citation"
      onClick={() => onOpenSource(source)}
    >
      <span className="inline-citation-phrase">{children}</span>
      <span className="sr-only">, citation {source.ordinal}</span>
      <span className="inline-citation-mark" aria-hidden="true">
        <Quote className="inline-citation-glyph" />
      </span>
    </button>
  );
}
