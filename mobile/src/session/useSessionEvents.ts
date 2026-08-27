import { useEffect, useMemo, useState } from "react";
import { listCodeTurns } from "../lib/api";
import { connectWithBackoff } from "../lib/machine";
import type { MachineClient } from "../lib/machine";
import {
  hydrateTurnHistory,
  initialTranscript,
  isSequencedCodeEventFrame,
  reduceTranscript,
  type TranscriptState,
} from "../lib/transcript";

export function useSessionEvents(
  client: MachineClient | null,
  sessionId: string | undefined,
  refreshVersion = 0,
): TranscriptState & { live: boolean } {
  const [state, setState] = useState<TranscriptState>(initialTranscript);
  const [live, setLive] = useState(false);

  useEffect(() => {
    if (!client || !sessionId) {
      setState(initialTranscript());
      setLive(false);
      return;
    }
    const activeClient = client;
    const activeSessionId = sessionId;
    let lastSeq = 0;
    let disposed = false;
    const requestedPrompts = new Set<string>();
    setState(initialTranscript());

    function hydrateTurns(turnId?: string) {
      void listCodeTurns(activeClient, activeSessionId)
        .then((turns) => {
          if (disposed) return;
          const selected = turnId
            ? turns.filter((turn) => turn.id === turnId)
            : turns;
          if (turnId && selected.length === 0) {
            requestedPrompts.delete(turnId);
            return;
          }
          for (const turn of selected) requestedPrompts.add(turn.id);
          setState((current) => hydrateTurnHistory(current, selected));
        })
        .catch(() => {
          if (turnId) requestedPrompts.delete(turnId);
        });
    }

    hydrateTurns();

    const conn = connectWithBackoff(
      () =>
        activeClient.openSocket(
          `/code/sessions/${encodeURIComponent(activeSessionId)}/events?after=${lastSeq}`,
        ),
      {
        onMessage: (data) => {
          try {
            const parsed: unknown = JSON.parse(data);
            if (!isSequencedCodeEventFrame(parsed)) return;
            if (
              parsed.event.type === "turn_started" &&
              !requestedPrompts.has(parsed.event.turn_id)
            ) {
              requestedPrompts.add(parsed.event.turn_id);
              hydrateTurns(parsed.event.turn_id);
            }
            setState((current) => {
              const next = reduceTranscript(current, parsed);
              lastSeq = next.lastSeq;
              return next;
            });
          } catch {
            // Drop malformed frames; reconnect resumes from lastSeq.
          }
        },
        onConnectionState: (status) => setLive(status === "live"),
      },
    );
    conn.start();
    return () => {
      disposed = true;
      conn.dispose();
    };
  }, [client, refreshVersion, sessionId]);

  return useMemo(() => ({ ...state, live }), [state, live]);
}
