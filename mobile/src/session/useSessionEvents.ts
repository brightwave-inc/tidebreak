import { useEffect, useMemo, useState } from "react";
import type { CodeTurnSnapshot } from "../generated/wire";
import { connectWithBackoff } from "../lib/machine";
import type { MachineClient } from "../lib/machine";
import {
  initialTranscript,
  isSequencedCodeEventFrame,
  reduceTranscript,
  type TranscriptState,
} from "../lib/transcript";

function isTurnSnapshot(value: unknown): value is CodeTurnSnapshot {
  return (
    !!value &&
    typeof value === "object" &&
    typeof (value as { id?: unknown }).id === "string" &&
    typeof (value as { user_input?: unknown }).user_input === "string"
  );
}

function parseTurns(json: unknown): CodeTurnSnapshot[] {
  if (Array.isArray(json)) return json.filter(isTurnSnapshot);
  if (json && typeof json === "object") {
    const record = json as Record<string, unknown>;
    for (const key of ["turns", "items"]) {
      const value = record[key];
      if (Array.isArray(value)) return value.filter(isTurnSnapshot);
    }
  }
  return [];
}

export function useSessionEvents(
  client: MachineClient | null,
  sessionId: string | undefined,
): TranscriptState & { live: boolean } {
  const [state, setState] = useState<TranscriptState>(initialTranscript);
  const [live, setLive] = useState(false);

  useEffect(() => {
    if (!client || !sessionId) {
      setState(initialTranscript());
      setLive(false);
      return;
    }
    let lastSeq = 0;
    setState(initialTranscript());

    void client
      .getJson(`/code/sessions/${encodeURIComponent(sessionId)}/turns`)
      .then((json) => {
        const turns = parseTurns(json);
        setState((current) => {
          if (current.items.some((item) => item.kind === "user")) return current;
          const items = turns.map((turn) => ({
            kind: "user" as const,
            id: `user:${turn.id}`,
            text: turn.user_input,
          }));
          return { ...current, items: [...items, ...current.items] };
        });
      })
      .catch(() => undefined);

    const conn = connectWithBackoff(
      () =>
        client.openSocket(
          `/code/sessions/${encodeURIComponent(sessionId)}/events?after=${lastSeq}`,
        ),
      {
        onMessage: (data) => {
          try {
            const parsed: unknown = JSON.parse(data);
            if (!isSequencedCodeEventFrame(parsed)) return;
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
    return () => conn.dispose();
  }, [client, sessionId]);

  return useMemo(() => ({ ...state, live }), [state, live]);
}
