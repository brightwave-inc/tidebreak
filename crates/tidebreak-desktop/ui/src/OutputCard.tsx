import { lazy, Suspense, useEffect, useRef, useState } from "react";

import type { ResultEntry } from "@/api";
import { DocumentIcon } from "@/components/document-table/DocumentIcon";
import { Skeleton } from "@/components/ui/skeleton";
import { readDeliverable, type DeliverablePreview } from "@/deliverables";
import { parseChartFigure } from "@/outputs/chartFigure";
import { usePanelNav } from "@/panel/usePanelNav";
import { TRANSCRIPT_RESULT_CARD_FRAME } from "@/TranscriptResultCard";
import { useActiveChatId } from "@/useActiveChatId";

// The plotting engine is a large dependency and most conversations never make a
// chart, so it is fetched from the app bundle on first use — the same lazy
// viewer the outputs panel draws a chart output with.
const ChartViewer = lazy(() => import("@/outputs/ChartViewer"));

const CHART_MEDIA_TYPE = "application/vnd.tidebreak.chart+json";
/** Matches the viewer's own default, so the placeholder holds the right space. */
const DEFAULT_CHART_HEIGHT = 400;

function normalizeMediaType(mediaType: string): string {
  return mediaType.split(";", 1)[0]!.trim().toLowerCase();
}

/**
 * The cards a phase hangs under itself for the outputs it published.
 *
 * One bordered card per created or updated output — type icon in a tinted chip,
 * filename, version line — and a click opens the output in the content panel,
 * where the document viewers (Univer for spreadsheets and docs, pdf.js for
 * PDFs) take over. The exec card's collapsed rail still names the command;
 * these cards carry the thing the reader actually came for.
 */
export function OutputCardList({ outputs }: { outputs: ResultEntry[] }) {
  if (outputs.length === 0) return null;
  return (
    <div className="flex flex-col items-start gap-2" aria-label="Created outputs">
      {outputs.map((entry) => (
        <OutputCard key={`${entry.targetId ?? entry.label}`} entry={entry} />
      ))}
    </div>
  );
}

function OpenOutputCard({
  entry,
  outputId,
}: {
  entry: ResultEntry;
  outputId: string;
}) {
  const { openPanel } = usePanelNav();
  return (
    <button
      type="button"
      className={`${TRANSCRIPT_RESULT_CARD_FRAME} hover:bg-muted/40 cursor-pointer transition-colors`}
      onClick={() => openPanel({ type: "outputs", outputId })}
      aria-label={`Open output ${entry.label}`}
    >
      <OutputCardBody entry={entry} />
    </button>
  );
}

function OutputCardBody({ entry }: { entry: ResultEntry }) {
  return (
    <>
      <span className="grid size-9 shrink-0 place-items-center" aria-hidden="true">
        <DocumentIcon mediaType={entry.mediaType} className="size-5" />
      </span>
      <span className="flex min-w-0 flex-col">
        <span className="truncate text-sm font-semibold">{entry.label}</span>
        {entry.meta && (
          <span className="text-muted-foreground text-xs tabular-nums">
            {entry.meta}
          </span>
        )}
      </span>
    </>
  );
}

/**
 * One published output. Clickable only when the projection carries the durable
 * output id; rows rehydrated from journals written before the id crossed still
 * render, as the same card without a destination.
 */
function OutputCard({ entry }: { entry: ResultEntry }) {
  const outputId = entry.targetId;

  if (outputId === null) {
    return (
      <div className={TRANSCRIPT_RESULT_CARD_FRAME}>
        <OutputCardBody entry={entry} />
      </div>
    );
  }

  if (normalizeMediaType(entry.mediaType ?? "") === CHART_MEDIA_TYPE) {
    return <ChartOutputCard entry={entry} outputId={outputId} />;
  }

  return <OpenOutputCard entry={entry} outputId={outputId} />;
}

/**
 * Reads the output's current revision once the card is on screen.
 *
 * The transcript can hold many of these, and a figure the reader has scrolled
 * past is not worth a round trip, so the fetch waits for the card to come into
 * view. Where there is no observer to ask, it simply reads on mount.
 */
function useVisiblePreview(
  chatId: string | null,
  outputId: string,
): { ref: React.RefObject<HTMLDivElement | null>; preview: DeliverablePreview | null; failed: boolean } {
  const ref = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(typeof IntersectionObserver !== "function");
  const [preview, setPreview] = useState<DeliverablePreview | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (visible) return;
    const element = ref.current;
    if (!element) return;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) setVisible(true);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [visible]);

  useEffect(() => {
    if (!visible) return;
    if (!chatId) {
      setFailed(true);
      return;
    }
    let cancelled = false;
    void readDeliverable(chatId, outputId)
      .then((next) => {
        if (!cancelled) setPreview(next);
      })
      .catch((error) => {
        // A chart that cannot be read is not an error worth a banner in the
        // transcript — the card still opens the output, which reports properly.
        console.error("inline chart could not be read", error);
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [chatId, outputId, visible]);

  return { ref, preview, failed };
}

/**
 * A chart output drawn where the turn produced it.
 *
 * A chart is only worth anything as a picture, so the transcript shows it
 * rather than a filename to click. The header still goes to the output panel,
 * which is where versions, export and the source view live. The current
 * revision is what gets drawn: the card names the version the turn wrote, and a
 * later turn that revises the same file is the one whose card shows the change.
 *
 * Anything that is not a drawable figure — an unreadable read, a truncated
 * file, bytes that are not a figure at all — falls back to the plain output
 * card, which is what every other output type shows here.
 */
function ChartOutputCard({
  entry,
  outputId,
}: {
  entry: ResultEntry;
  outputId: string;
}) {
  const chatId = useActiveChatId();
  const { openPanel } = usePanelNav();
  const { ref, preview, failed } = useVisiblePreview(chatId, outputId);

  const figure =
    preview && !preview.truncated ? parseChartFigure(preview.content) : null;

  const header = (
    <button
      type="button"
      className="hover:bg-muted/40 flex w-full min-w-0 cursor-pointer items-center gap-3 rounded-t-lg px-4 py-3 text-left transition-colors"
      onClick={() => openPanel({ type: "outputs", outputId })}
      aria-label={`Open output ${entry.label}`}
    >
      <OutputCardBody entry={entry} />
    </button>
  );

  if (failed || (preview && !figure)) {
    return (
      <div ref={ref}>
        <OpenOutputCard entry={entry} outputId={outputId} />
      </div>
    );
  }

  return (
    <div
      ref={ref}
      className="bg-background w-full max-w-2xl min-w-0 rounded-lg border shadow-sm"
    >
      {header}
      <div className="px-4 pb-4">
        {preview ? (
          <Suspense fallback={<ChartPlaceholder />}>
            <ChartViewer preview={preview} />
          </Suspense>
        ) : (
          <ChartPlaceholder />
        )}
      </div>
    </div>
  );
}

function ChartPlaceholder() {
  return (
    <Skeleton className="w-full" style={{ height: DEFAULT_CHART_HEIGHT }} />
  );
}
