import type { ResultEntry } from "@/api";
import { DocumentIcon } from "@/components/document-table/DocumentIcon";
import { usePanelNav } from "@/panel/usePanelNav";

/**
 * The cards a phase hangs under itself for the outputs it published.
 *
 * Mirrors Brightwave's deliverable cards at the end of a turn: one bordered
 * card per created or updated output — type icon in a tinted chip, filename,
 * version line — and a click opens the output in the content panel, where the
 * document viewers (Univer for spreadsheets and docs, pdf.js for PDFs) take
 * over. The exec card's collapsed rail still names the command; these cards
 * carry the thing the reader actually came for.
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

/**
 * One published output. Clickable only when the projection carries the durable
 * output id; rows rehydrated from journals written before the id crossed still
 * render, as the same card without a destination.
 */
function OutputCard({ entry }: { entry: ResultEntry }) {
  const { openPanel } = usePanelNav();
  const outputId = entry.targetId;
  const body = (
    <>
      <span className="bg-muted rounded-md p-2" aria-hidden="true">
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
  const frame =
    "bg-background inline-flex max-w-full min-w-0 items-center gap-3 rounded-lg border px-4 py-3 text-left shadow-sm";
  if (outputId === null) {
    return <div className={frame}>{body}</div>;
  }
  return (
    <button
      type="button"
      className={`${frame} hover:bg-muted/40 cursor-pointer transition-colors`}
      onClick={() => openPanel({ type: "outputs", outputId })}
      aria-label={`Open output ${entry.label}`}
    >
      {body}
    </button>
  );
}
