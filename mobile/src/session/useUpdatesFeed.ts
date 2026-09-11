import { useCallback, useEffect, useRef, useState } from "react";
import type { MachineClient } from "../lib/machine";
import { acquireUpdatesFeed } from "./updatesFeed";
import { useUpdatesStore } from "./updatesStore";

export function useUpdatesFeed(client: MachineClient | null): {
  live: boolean;
  refresh: () => void;
} {
  const reset = useUpdatesStore((state) => state.reset);
  const [live, setLive] = useState(false);
  const refreshRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    if (!client) {
      reset();
      setLive(false);
      return;
    }
    const handle = acquireUpdatesFeed(client, (state) =>
      setLive(state === "live"),
    );
    setLive(handle.state === "live");
    refreshRef.current = handle.refresh;
    return () => {
      refreshRef.current = null;
      handle.release();
    };
  }, [client, reset]);

  const refresh = useCallback(() => {
    refreshRef.current?.();
  }, []);

  return { live, refresh };
}
