import { createContext, useContext, useMemo, useRef } from "react";

import type { PanelContent } from "./panelTypes";

/** Where a citation points: one place inside one source. */
export type CitationTarget = {
  documentId: string;
  citationId: string;
};

export type SourceNav = {
  openCitation: (target: CitationTarget) => void;
  openDocument: (documentId: string) => void;
};

const SourceNavContext = createContext<SourceNav | null>(null);

export const SourceNavProvider = SourceNavContext.Provider;

/**
 * How a citation reaches the source panel beside it, or `null` where there is
 * no panel to reach.
 *
 * A citation is rendered several components below the route that owns the
 * layout, and most of what sits in between has no business knowing about
 * panels — so this is read where the citation is rather than threaded through
 * each of them. Read outside a provider it is `null`, and a citation renders as
 * it always has.
 */
export function useSourceNav(): SourceNav | null {
  return useContext(SourceNavContext);
}

/**
 * A `SourceNav` whose identity survives a re-render.
 *
 * The transcript re-renders on every streamed token, and a context value that
 * changed with it would re-render every settled message alongside the live one,
 * defeating the memoization that keeps streaming cheap. Only the latest way to
 * navigate is needed, and only at the moment of a click, so it is read through
 * a ref instead of captured.
 */
export function useStableSourceNav(
  openPanel: (panel: PanelContent) => void,
): SourceNav {
  const latest = useRef(openPanel);
  latest.current = openPanel;
  return useMemo(
    () => ({
      openCitation: ({ documentId, citationId }: CitationTarget) =>
        latest.current({ type: "document", documentId, citationId }),
      openDocument: (documentId: string) =>
        latest.current({ type: "document", documentId }),
    }),
    [],
  );
}
