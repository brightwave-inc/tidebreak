import { useCallback, useEffect, useRef, useState } from "react";

import type {
  ComputerUsePermissionHost,
  ComputerUsePermissionPane,
  ComputerUsePermissionStatus,
} from "@/computerUsePermissions";
import { useComputerUsePermissionAsk } from "@/computerUsePermissionAsk";

export type PermissionErrorKind =
  | "status"
  | "request"
  | "open_settings"
  | "restart";

/**
 * What went wrong, in the words of the surface it went wrong on.
 *
 * `refreshable` is whether the caller draws a Refresh button. Settings does,
 * so its copy sends the reader to it; the setup dialog re-reads on window
 * focus instead and must not name a control that is not there.
 */
export function permissionErrorMessage(
  kind: PermissionErrorKind,
  refreshable: boolean,
): string {
  switch (kind) {
    case "status":
      return refreshable
        ? "macOS permission status could not be checked. Refresh to try again. If the native helper stays unavailable, restart Tidebreak."
        : "macOS permission status could not be checked. If the native helper stays unavailable, restart Tidebreak.";
    case "request":
      return refreshable
        ? "macOS permissions could not be requested. Open System Settings to enable them, then refresh."
        : "macOS permissions could not be requested. Open System Settings to enable them.";
    case "open_settings":
      return "System Settings could not be opened. Open Privacy & Security in System Settings and select the permission.";
    case "restart":
      return "Tidebreak could not restart. Quit Tidebreak and open it again.";
  }
}

export interface ComputerUsePermissions {
  availability: ReturnType<ComputerUsePermissionHost["availability"]>;
  status: ComputerUsePermissionStatus | null;
  /** The status once it names the two grants, so callers skip the narrowing. */
  permissions: Extract<
    ComputerUsePermissionStatus,
    { status: "available" }
  > | null;
  /** At least one grant is still missing. `false` until a status arrives. */
  missing: boolean;
  error: string | null;
  loading: boolean;
  requesting: boolean;
  opening: ComputerUsePermissionPane | null;
  /** A request or a settings launch is in flight. */
  busy: boolean;
  /**
   * Screen Recording is still off after macOS was asked for it in this run.
   * macOS applies that grant only to a process started after it, so if the
   * person turned it on, Tidebreak has to restart before it takes effect.
   */
  restartSuggested: boolean;
  restarting: boolean;
  refresh: () => Promise<void>;
  request: () => Promise<void>;
  openSettings: (pane: ComputerUsePermissionPane) => Promise<void>;
  restart: () => Promise<void>;
}

/**
 * The macOS grant state and the three ways to move it: read, request, and open
 * the System Settings pane.
 *
 * Reads race each other and race the request, because the window regains focus
 * the moment the person comes back from System Settings — often while the
 * request they started is still open. Generation counters settle those races so
 * a stale read never overwrites a newer answer, and a failed request keeps its
 * error rather than losing it to a read that resolved behind it.
 */
export function useComputerUsePermissions(
  host: ComputerUsePermissionHost,
  { refreshable = true }: { refreshable?: boolean } = {},
): ComputerUsePermissions {
  const availability = host.availability();
  const [status, setStatus] = useState<ComputerUsePermissionStatus | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(availability === "local");
  const [requesting, setRequesting] = useState(false);
  const [opening, setOpening] = useState<ComputerUsePermissionPane | null>(
    null,
  );
  const [restarting, setRestarting] = useState(false);
  const screenRecordingRequested = useComputerUsePermissionAsk(
    (state) => state.screenRecordingRequested,
  );
  const noteScreenRecordingRequested = useComputerUsePermissionAsk(
    (state) => state.noteScreenRecordingRequested,
  );
  const generation = useRef(0);
  const requestGeneration = useRef(0);
  const mounted = useRef(false);

  const refresh = useCallback(async () => {
    if (availability !== "local") return;
    const current = ++generation.current;
    setLoading(true);
    try {
      const next = await host.status();
      if (!mounted.current || current !== generation.current) return;
      setStatus(next);
      setError(null);
    } catch {
      if (!mounted.current || current !== generation.current) return;
      setStatus(null);
      setError(permissionErrorMessage("status", refreshable));
    } finally {
      if (mounted.current && current === generation.current) setLoading(false);
    }
  }, [availability, host, refreshable]);

  useEffect(() => {
    mounted.current = true;
    setRequesting(false);
    void refresh();
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => {
      mounted.current = false;
      generation.current += 1;
      requestGeneration.current += 1;
      window.removeEventListener("focus", onFocus);
    };
  }, [refresh]);

  const request = useCallback(async () => {
    const current = ++generation.current;
    const requestId = ++requestGeneration.current;
    const isCurrentRequest = () =>
      mounted.current && requestId === requestGeneration.current;
    setLoading(false);
    setRequesting(true);
    setError(null);
    try {
      const next = await host.request();
      // macOS has now asked, so a Screen Recording grant made from here on
      // needs a restart to apply. A failed request asked nothing.
      noteScreenRecordingRequested();
      if (!isCurrentRequest()) return;
      if (current === generation.current) {
        setStatus(next);
        setLoading(false);
      } else {
        // A focus read can observe permissions before the request completes.
        await refresh();
      }
    } catch {
      if (isCurrentRequest()) {
        // An older status read must not clear this request's error.
        generation.current += 1;
        setLoading(false);
        setError(permissionErrorMessage("request", refreshable));
      }
    } finally {
      if (isCurrentRequest()) setRequesting(false);
    }
  }, [host, refresh, refreshable, noteScreenRecordingRequested]);

  const openSettings = useCallback(
    async (pane: ComputerUsePermissionPane) => {
      setOpening(pane);
      setError(null);
      try {
        await host.openSettings(pane);
        // The person can turn Screen Recording on from here, and that grant
        // applies only after a restart.
        if (pane === "screen_recording") noteScreenRecordingRequested();
      } catch {
        if (mounted.current)
          setError(permissionErrorMessage("open_settings", refreshable));
      } finally {
        if (mounted.current) setOpening(null);
      }
    },
    [host, refreshable, noteScreenRecordingRequested],
  );

  const restart = useCallback(async () => {
    setRestarting(true);
    setError(null);
    try {
      // Resolves once the shell has taken the request: at once when nothing
      // is working, or after the quit prompt when something is and the
      // person cancels it.
      await host.restart();
    } catch {
      if (mounted.current)
        setError(permissionErrorMessage("restart", refreshable));
    } finally {
      if (mounted.current) setRestarting(false);
    }
  }, [host, refreshable]);

  const permissions = status?.status === "available" ? status : null;
  return {
    availability,
    status,
    permissions,
    missing: Boolean(
      permissions &&
        (!permissions.accessibility || !permissions.screenRecording),
    ),
    error,
    loading,
    requesting,
    opening,
    busy: requesting || opening !== null || restarting,
    restartSuggested: Boolean(
      permissions && !permissions.screenRecording && screenRecordingRequested,
    ),
    restarting,
    refresh,
    request,
    openSettings,
    restart,
  };
}
