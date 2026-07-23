import { useCallback, useEffect, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

const UPDATE_STATE_EVENT = "desktop-update-state";
const UPDATE_CONTROL_ERROR = "Update controls are temporarily unavailable.";
const UPDATE_RESTART_ERROR = "Could not restart OpenWave. Try again.";

export type DesktopUpdateState = {
  status: "idle" | "checking" | "downloading" | "ready";
  version: string | null;
  error: string | null;
  enabled: boolean;
};

export type DesktopUpdatesController = {
  state: DesktopUpdateState;
  check: () => Promise<DesktopUpdateState>;
  restart: () => Promise<void>;
};

export const INITIAL_UPDATE_STATE: DesktopUpdateState = {
  status: "idle",
  version: null,
  error: null,
  enabled: false,
};

function unavailableUpdateState(): DesktopUpdateState {
  return {
    ...INITIAL_UPDATE_STATE,
    enabled: isTauri(),
    error: UPDATE_CONTROL_ERROR,
  };
}

async function getDesktopUpdateState(): Promise<DesktopUpdateState> {
  if (!isTauri()) return INITIAL_UPDATE_STATE;
  try {
    return await invoke<DesktopUpdateState>("desktop_update_state");
  } catch {
    return unavailableUpdateState();
  }
}

async function checkForDesktopUpdate(): Promise<DesktopUpdateState> {
  if (!isTauri()) return INITIAL_UPDATE_STATE;
  try {
    return await invoke<DesktopUpdateState>("check_for_update");
  } catch {
    return unavailableUpdateState();
  }
}

async function restartForDesktopUpdate(): Promise<void> {
  if (!isTauri()) return;
  await invoke("restart_for_update");
}

export function useDesktopUpdates(): DesktopUpdatesController {
  const [state, setState] = useState<DesktopUpdateState>(INITIAL_UPDATE_STATE);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;

    void (async () => {
      try {
        unlisten = await listen<DesktopUpdateState>(
          UPDATE_STATE_EVENT,
          (event) => {
            if (!cancelled) setState(event.payload);
          },
        );
        const current = await getDesktopUpdateState();
        if (!cancelled) setState(current);
      } catch {
        if (!cancelled) {
          setState(unavailableUpdateState());
        }
      }
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const check = useCallback(async () => {
    setState((current) =>
      current.enabled
        ? { ...current, status: "checking", error: null }
        : current,
    );
    const next = await checkForDesktopUpdate();
    setState(next);
    return next;
  }, []);

  const restart = useCallback(async () => {
    try {
      await restartForDesktopUpdate();
    } catch {
      setState((current) => ({
        ...current,
        error: UPDATE_RESTART_ERROR,
      }));
    }
  }, []);

  return { state, check, restart };
}
