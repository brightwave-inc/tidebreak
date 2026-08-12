import { useEffect, useRef, useState } from "react";

import type { AgentRunProgress, AgentRunProgressEntry } from "./api";
import { formatActivityTime } from "./AgentActivityTimeline";
import { cn } from "@/lib/utils";

/** How often a live run is asked what it has published since the last page. */
const PROGRESS_POLL_INTERVAL_MS = 5_000;

/**
 * How many lines one reader keeps.
 *
 * The server bounds the stream by retention; this bounds what a single open
 * panel accumulates over a long run, so a chatty agent cannot grow the DOM
 * without limit. The newest lines are the ones worth keeping.
 */
const MAX_RETAINED_LINES = 200;

export type AgentRunProgressState = {
  loading: boolean;
  error: boolean;
  loaded: boolean;
  entries: AgentRunProgressEntry[];
  /** The most recent line, which is what a status row shows. */
  latest: AgentRunProgressEntry | null;
};

/** The held stream, plus the run it is an answer about. */
type HeldProgress = {
  runId: string | null;
  loading: boolean;
  error: boolean;
  loaded: boolean;
  entries: AgentRunProgressEntry[];
};

const NO_PROGRESS_HELD: HeldProgress = {
  runId: null,
  loading: false,
  error: false,
  loaded: false,
  entries: [],
};

/**
 * A background run's live progress, resumed from the cursor the reader already
 * holds.
 *
 * Every poll asks only for what has arrived since the last page, so a run that
 * has published nothing new answers with an empty page rather than the whole
 * stream. The cursor is held per run and never rewound: re-reading from zero
 * would re-render lines already on screen and grow with the run.
 *
 * Retention bounds the stream server-side, so a reader that arrives late — or
 * comes back after a long absence — may find the oldest lines gone. A jump in
 * `sequence` is therefore normal and says nothing went wrong; only the order
 * is a contract.
 *
 * `enabled` gates reading at all; `live` decides whether the read repeats. A
 * settled run publishes nothing further, so it is read once and left alone.
 */
export function useAgentRunProgress(
  runId: string | null,
  enabled: boolean,
  live: boolean,
  loadProgress: (
    runId: string,
    afterSequence: number,
  ) => Promise<AgentRunProgress>,
): AgentRunProgressState {
  const [held, setHeld] = useState<HeldProgress>(NO_PROGRESS_HELD);
  // The cursor outlives the effect: `live` flipping, or the loader identity
  // changing, must not send the next poll back to the start of the stream.
  const cursor = useRef<{ runId: string | null; sequence: number }>({
    runId: null,
    sequence: 0,
  });

  useEffect(() => {
    if (!enabled || runId === null) return;
    if (cursor.current.runId !== runId) {
      cursor.current = { runId, sequence: 0 };
    }
    let cancelled = false;

    setHeld((current) =>
      current.runId === runId
        ? { ...current, loading: !current.loaded, error: false }
        : { ...NO_PROGRESS_HELD, runId, loading: true },
    );

    const read = async () => {
      const from = cursor.current.sequence;
      try {
        const page = await loadProgress(runId, from);
        if (cancelled || cursor.current.runId !== runId) return;
        cursor.current = {
          runId,
          sequence: Math.max(page.nextSequence, cursor.current.sequence),
        };
        setHeld((current) => {
          const base = current.runId === runId ? current.entries : [];
          const lastHeld = base[base.length - 1]?.sequence ?? -Infinity;
          const fresh = page.entries.filter(
            (entry) => entry.sequence > lastHeld,
          );
          return {
            runId,
            loading: false,
            error: false,
            loaded: true,
            entries:
              fresh.length === 0
                ? base
                : [...base, ...fresh].slice(-MAX_RETAINED_LINES),
          };
        });
      } catch {
        // A failed page is not evidence the stream is gone: whatever is held
        // keeps rendering and the next poll resumes from the same cursor.
        if (!cancelled) {
          setHeld((current) =>
            current.runId === runId
              ? { ...current, loading: false, error: true }
              : current,
          );
        }
      }
    };

    void read();
    if (!live) {
      return () => {
        cancelled = true;
      };
    }
    const interval = window.setInterval(() => void read(), PROGRESS_POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [enabled, runId, live, loadProgress]);

  // Answers about another run are not this run's to show, not even for the one
  // commit before the effect resets them.
  const current = held.runId === runId ? held : NO_PROGRESS_HELD;
  return {
    loading: current.loading,
    error: current.error,
    loaded: current.loaded,
    entries: current.entries,
    latest: current.entries[current.entries.length - 1] ?? null,
  };
}

/**
 * The stream in full, beside the run's activity history.
 *
 * The lines are the agent's own prose — the same class of text the task plan's
 * steps carry — so they are rendered as text and wrap rather than being
 * clipped, and nothing in them is interpreted as markup.
 */
export function AgentRunProgressStream({
  state,
  className,
}: {
  state: AgentRunProgressState;
  className?: string;
}) {
  if (state.entries.length === 0) {
    // An empty stream is ordinary: many runs only leave tool activity, not
    // free-form narration. Stay quiet so the activity timeline below is not
    // undercut by a "no progress" line while work is clearly underway.
    if (state.loading && !state.loaded) {
      return (
        <p
          className={cn("text-xs text-muted-foreground", className)}
          role="status"
        >
          Loading progress…
        </p>
      );
    }
    if (state.error) {
      return (
        <p
          className={cn("text-xs text-muted-foreground", className)}
          role="status"
        >
          Progress is unavailable.
        </p>
      );
    }
    return null;
  }

  return (
    <section className={cn("flex flex-col gap-1.5", className)} aria-label="Progress">
      <p className="text-[11px] font-semibold uppercase tracking-[0.06em] text-muted-foreground">
        Progress
      </p>
      <ol className="flex flex-col gap-1.5 text-xs" role="list">
        {state.entries.map((entry) => (
          <li key={entry.sequence} className="flex min-w-0 items-start gap-2">
            <time
              className="shrink-0 tabular-nums text-muted-foreground"
              dateTime={entry.at}
            >
              {formatActivityTime(entry.at)}
            </time>
            <span className="min-w-0 break-words text-foreground">
              {entry.text}
            </span>
          </li>
        ))}
      </ol>
    </section>
  );
}
