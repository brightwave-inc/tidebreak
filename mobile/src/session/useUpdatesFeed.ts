import { useCallback, useEffect, useRef, useState } from "react";
import type { MachineClient } from "../lib/machine";
import { connectWithBackoff, type ReconnectingSocket } from "../lib/machine";
import { isCodeUpdateNotice, noticeToAction } from "../lib/updates";
import { useUpdatesStore } from "./updatesStore";

export function useUpdatesFeed(client: MachineClient | null): {
  live: boolean;
  refresh: () => void;
} {
  const apply = useUpdatesStore((state) => state.apply);
  const reset = useUpdatesStore((state) => state.reset);
  const [live, setLive] = useState(false);
  const connRef = useRef<ReconnectingSocket | null>(null);

  useEffect(() => {
    if (!client) {
      reset();
      setLive(false);
      return;
    }
    const conn = connectWithBackoff(
      () => client.openSocket("/code/updates"),
      {
        onMessage: (data) => {
          try {
            const parsed: unknown = JSON.parse(data);
            if (!isCodeUpdateNotice(parsed)) return;
            const action = noticeToAction(parsed);
            if (action) apply(action);
          } catch {
            // Drop malformed notices; the next snapshot heals the list.
          }
        },
        onConnectionState: (state) => setLive(state === "live"),
      },
    );
    connRef.current = conn;
    conn.start();
    return () => {
      conn.dispose();
      connRef.current = null;
      reset();
    };
  }, [apply, client, reset]);

  const refresh = useCallback(() => {
    connRef.current?.refresh();
  }, []);

  return { live, refresh };
}
