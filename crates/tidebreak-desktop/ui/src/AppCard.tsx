import { LayoutGrid } from "lucide-react";
import { useNavigate } from "@tanstack/react-router";

import type { ResultEntry } from "@/api";
import { Button } from "@/components/ui/button";
import { TRANSCRIPT_RESULT_CARD_FRAME } from "@/TranscriptResultCard";

/**
 * The cards a phase hangs under itself for the apps it published.
 *
 * The sibling of the output cards, and for the same reason: what the reader
 * came for is the app, and the activity rail it was announced in is collapsed
 * by default. The app the turn just built must be one click from the message
 * that built it, not from a library the reader has to go find.
 */
export function AppCardList({ apps }: { apps: ResultEntry[] }) {
  if (apps.length === 0) return null;
  return (
    <div className="flex flex-col items-start gap-2" aria-label="Created apps">
      {apps.map((entry) => (
        <AppCard key={`${entry.targetId ?? entry.label}`} entry={entry} />
      ))}
    </div>
  );
}

/**
 * One published app: its name, which revision this call published, and the way
 * in. The app opens on its library page, where its consent sheet already
 * lives — nothing here grants anything.
 *
 * The action is present only when the projection carries the app id; a row
 * rehydrated from a journal written before the id crossed still renders, as
 * the same card without a destination.
 */
function AppCard({ entry }: { entry: ResultEntry }) {
  const navigate = useNavigate();
  const appId = entry.targetId;
  return (
    <div className={TRANSCRIPT_RESULT_CARD_FRAME}>
      <span
        className="grid size-9 shrink-0 place-items-center"
        aria-hidden="true"
      >
        <LayoutGrid className="text-icon-blue size-5" />
      </span>
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="truncate text-sm font-semibold">{entry.label}</span>
        {entry.meta && (
          <span className="text-muted-foreground text-xs tabular-nums">
            {entry.meta}
          </span>
        )}
      </span>
      {appId !== null && (
        <Button
          type="button"
          size="sm"
          variant="outline"
          className="ml-2 shrink-0"
          onClick={() =>
            void navigate({ to: "/apps/$appId", params: { appId } })
          }
          aria-label={`Open app ${entry.label}`}
        >
          Open app
        </Button>
      )}
    </div>
  );
}
