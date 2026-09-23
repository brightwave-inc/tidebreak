import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

const UPDATE_STATE_EVENT = "desktop-update-state";
/** Raised by the native "Check for Updates…" menu item. */
export const UPDATE_CHECK_REQUESTED_EVENT = "desktop-update-check-requested";
const UPDATE_CONTROL_ERROR = "Update controls are temporarily unavailable.";
const UPDATE_RESTART_ERROR = "Could not restart Tidebreak. Try again.";
const UPDATE_PREFERENCE_ERROR = "Could not save the setting. Try again.";

export type DesktopUpdateState = {
  /**
   * `available` means a newer release is published but not downloaded yet,
   * because automatic downloads are off.
   */
  status: "idle" | "checking" | "available" | "downloading" | "ready";
  version: string | null;
  error: string | null;
  enabled: boolean;
};

export type DesktopUpdatesController = {
  state: DesktopUpdateState;
  check: () => Promise<DesktopUpdateState>;
  restart: () => Promise<void>;
  /** The most recent explicit check confirmed the app is current. */
  upToDate: boolean;
};

/** Whether Tidebreak downloads a published update without asking. */
export type DesktopUpdatePreferences = {
  automaticDownloads: boolean;
  /** Your organization's managed policy sets `automaticDownloads`. */
  managed: boolean;
};

export const INITIAL_UPDATE_STATE: DesktopUpdateState = {
  status: "idle",
  version: null,
  error: null,
  enabled: false,
};

/** What the setting reads before the desktop reports it, and outside one. */
export const DEFAULT_UPDATE_PREFERENCES: DesktopUpdatePreferences = {
  automaticDownloads: true,
  managed: false,
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

/**
 * Download the published update now, whatever the automatic-download setting
 * says. Progress and the result arrive as update-state events, so every
 * surface that shows the update follows along.
 */
export async function downloadDesktopUpdate(): Promise<DesktopUpdateState> {
  if (!isTauri()) return INITIAL_UPDATE_STATE;
  try {
    return await invoke<DesktopUpdateState>("download_update");
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
  const [upToDate, setUpToDate] = useState(false);

  // "Up to date" is a claim about the last explicit check, not a standing
  // state; anything that contradicts it (a staged update, an error, a new
  // check in flight) clears it.
  useEffect(() => {
    if (state.status !== "idle" || state.error) setUpToDate(false);
  }, [state.error, state.status]);

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
    setUpToDate(false);
    setState((current) =>
      current.enabled
        ? { ...current, status: "checking", error: null }
        : current,
    );
    const next = await checkForDesktopUpdate();
    setState(next);
    setUpToDate(next.enabled && next.status === "idle" && !next.error);
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

  return { state, check, restart, upToDate };
}

/**
 * The automatic-download setting, saved as soon as it changes.
 *
 * `preferences` is `null` until the desktop reports it. Outside the desktop
 * there is nothing to save, so it reads as the default.
 */
export function useDesktopUpdatePreferences(): {
  preferences: DesktopUpdatePreferences | null;
  saving: boolean;
  error: string | null;
  setAutomaticDownloads: (enabled: boolean) => Promise<void>;
} {
  const [preferences, setPreferences] =
    useState<DesktopUpdatePreferences | null>(() =>
      isTauri() ? null : DEFAULT_UPDATE_PREFERENCES,
    );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const preferencesRef = useRef(preferences);
  preferencesRef.current = preferences;

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    void invoke<DesktopUpdatePreferences>("desktop_update_preferences").then(
      (value) => {
        if (!cancelled) setPreferences(value);
      },
      () => {
        if (!cancelled) setPreferences(DEFAULT_UPDATE_PREFERENCES);
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  const setAutomaticDownloads = useCallback(async (enabled: boolean) => {
    const previous = preferencesRef.current;
    if (!isTauri() || !previous) return;
    setError(null);
    setSaving(true);
    setPreferences({ ...previous, automaticDownloads: enabled });
    try {
      setPreferences(
        await invoke<DesktopUpdatePreferences>(
          "set_automatic_update_downloads",
          { enabled },
        ),
      );
    } catch (reason) {
      setPreferences(previous);
      setError(typeof reason === "string" ? reason : UPDATE_PREFERENCE_ERROR);
    } finally {
      setSaving(false);
    }
  }, []);

  return { preferences, saving, error, setAutomaticDownloads };
}
