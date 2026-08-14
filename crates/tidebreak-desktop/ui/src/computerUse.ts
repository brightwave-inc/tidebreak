import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

/** The app most recently under control, with its idle re-arm window. */
export type ComputerUseActiveControl = {
  bundleId: string;
  appName: string | null;
  lastActivityMillis: number;
  visibleUntilMillis: number;
};

/** The whole native computer-use surface the shell may render. */
export type ComputerUseSnapshot = {
  active: ComputerUseActiveControl | null;
  halted: boolean;
};

const STATE_EVENT = "computer-use-state-changed";

const EMPTY: ComputerUseSnapshot = {
  active: null,
  halted: false,
};

/**
 * The live computer-use surface: which app Tidebreak is driving, whether the
 * user stopped it, and the asks waiting on a decision. The listener attaches
 * before the snapshot query runs, so a state change emitted while the query
 * is in flight is delivered to the handler rather than lost; a query that
 * resolves after an event would be older state and must not overwrite it.
 */
export function useComputerUseState(): ComputerUseSnapshot {
  const [snapshot, setSnapshot] = useState<ComputerUseSnapshot>(EMPTY);
  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let sawEvent = false;
    let unlisten: UnlistenFn | undefined;
    listen<ComputerUseSnapshot>(STATE_EVENT, (event) => {
      sawEvent = true;
      setSnapshot(event.payload);
    })
      .then((stop) => {
        if (cancelled) {
          stop();
          return;
        }
        unlisten = stop;
        invoke<ComputerUseSnapshot>("computer_use_state")
          .then((initial) => {
            if (!cancelled && !sawEvent) setSnapshot(initial);
          })
          .catch(() => {});
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
  return snapshot;
}

/** The Stop button: halt control before the next broker round-trip. */
export function stopComputerUseControl(): Promise<void> {
  return invoke("stop_computer_use_control");
}

/** Re-arm control after a Stop. */
export function resumeComputerUseControl(): Promise<void> {
  return invoke("resume_computer_use_control");
}
