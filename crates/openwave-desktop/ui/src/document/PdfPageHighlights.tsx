import type { CSSProperties } from "react";
import { useEffect, useRef } from "react";

import type { CitationPageBounds } from "@/api";
import {
  CITATION_MARK_CLASS,
  CITATION_MARK_STYLE,
} from "@/components/document/citationMark";
import { cn } from "@/lib/utils";

/**
 * The fixed-point scale page bounds arrive in: coordinates are ten-thousandths
 * of the page box, so a rectangle divided by this is the fraction of the page
 * it covers. Mirrors `PAGE_BOUNDS_SCALE` on the server.
 */
const PAGE_BOUNDS_SCALE = 10_000;

/**
 * A box over the page reads as the cited passage does everywhere else, which is
 * why it takes the shared mark style rather than a colour of its own. What it
 * adds is the blend: the fill is multiplied into the page so the words beneath
 * it stay legible instead of being painted over, and it takes no pointer events
 * so selecting and following links on the page carries on underneath.
 */
const HIGHLIGHT_STYLE = "pointer-events-none absolute mix-blend-multiply";

type Props = {
  /** The page currently on screen. Rectangles on any other page are not drawn. */
  page: number;
  /** Every rectangle of the citation, across all the pages it covers. */
  highlights: readonly CitationPageBounds[];
  /** Follow the passage onto the next page that carries part of it. */
  onNavigate: (page: number) => void;
};

/**
 * A citation's rectangles, drawn over the page they were recorded on.
 *
 * Positioned in percentages of the page container rather than in pixels, so
 * zooming or resizing the panel moves the boxes with the page for free — the
 * rectangles are fractions of the page box and never needed the rendered size.
 *
 * Mount this only once the page itself has rendered: the first box is scrolled
 * to on mount, and against pdf.js's loading placeholder that would centre on a
 * position the page does not have yet.
 */
export function PdfPageHighlights({ page, highlights, onNavigate }: Props) {
  const onThisPage = highlights.filter((rect) => rect.page === page);
  const firstRef = useRef<HTMLDivElement | null>(null);

  // Reveal the passage once for the page it starts on. Keyed on the page and
  // the box rather than on the element, so re-rendering the page at a new zoom
  // does not yank the reader back to the citation mid-gesture.
  const anchor = onThisPage[0];
  const anchorKey = anchor
    ? `${page}:${anchor.bounds.top}:${anchor.bounds.left}`
    : null;
  useEffect(() => {
    if (anchorKey === null) return;
    firstRef.current?.scrollIntoView({ block: "center", inline: "center" });
  }, [anchorKey]);

  if (onThisPage.length === 0) return null;

  const continuesOn = nextPageWithBounds(highlights, page);
  const last = onThisPage[onThisPage.length - 1]!;

  return (
    <>
      {onThisPage.map((rect, index) => (
        <div
          key={`${rect.page}:${index}`}
          ref={index === 0 ? firstRef : undefined}
          // Decoration over a canvas: the passage itself is in the page's text
          // layer, and announcing an empty box beside it says nothing.
          aria-hidden="true"
          className={cn(CITATION_MARK_CLASS, CITATION_MARK_STYLE, HIGHLIGHT_STYLE)}
          style={highlightBoxStyle(rect.bounds)}
        />
      ))}
      {continuesOn != null && (
        <button
          type="button"
          onClick={() => onNavigate(continuesOn)}
          className="absolute z-10 -translate-x-full rounded-md border bg-popover px-2 py-1 text-xs font-medium whitespace-nowrap text-popover-foreground shadow-sm"
          style={continuationStyle(last.bounds)}
        >
          Continues on next page →
        </button>
      )}
    </>
  );
}

/**
 * Where one rectangle sits on the page, as percentages of the page container.
 *
 * The couple of pixels of slack around each box is deliberate: a rectangle
 * fitted tightly to a line of text clips its descenders, and the highlight then
 * reads as cutting through the words it is meant to mark.
 */
export function highlightBoxStyle(bounds: CitationPageBounds["bounds"]): CSSProperties {
  return {
    left: `calc(${percent(bounds.left)}% - 2px)`,
    top: `calc(${percent(bounds.top)}% - 2px)`,
    width: `calc(${percent(bounds.width)}% + 7px)`,
    height: `calc(${percent(bounds.height)}% + 7px)`,
  };
}

/** The affordance hangs off the bottom-right corner of the last box drawn. */
function continuationStyle(bounds: CitationPageBounds["bounds"]): CSSProperties {
  return {
    left: `${percent(bounds.left + bounds.width)}%`,
    top: `calc(${percent(bounds.top + bounds.height)}% + 4px)`,
  };
}

/**
 * The next page carrying part of this citation, or null where the passage ends
 * on the page on screen. Not simply the following page: what the reader is
 * being offered is the rest of the passage, wherever it resumes.
 */
function nextPageWithBounds(
  highlights: readonly CitationPageBounds[],
  page: number,
): number | null {
  const later = highlights
    .map((rect) => rect.page)
    .filter((candidate) => candidate > page);
  return later.length > 0 ? Math.min(...later) : null;
}

function percent(fixedPoint: number): number {
  return (fixedPoint / PAGE_BOUNDS_SCALE) * 100;
}
