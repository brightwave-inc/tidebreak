import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  computerUsePermissionHost,
  type ComputerUsePermissionHost,
  type ComputerUsePermissionPane,
  type ComputerUsePermissionStatus,
} from "@/computerUsePermissions";
import { SettingsError, SettingsSection } from "./primitives";

export function ComputerUsePermissionsSection({
  host = computerUsePermissionHost,
}: {
  host?: ComputerUsePermissionHost;
}) {
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
  const generation = useRef(0);
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
      setError(
        "macOS permission status could not be checked. Refresh to try again. If the native helper stays unavailable, restart Tidebreak.",
      );
    } finally {
      if (mounted.current && current === generation.current) setLoading(false);
    }
  }, [availability, host]);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => {
      mounted.current = false;
      generation.current += 1;
      window.removeEventListener("focus", onFocus);
    };
  }, [refresh]);

  async function request() {
    const current = ++generation.current;
    setLoading(false);
    setRequesting(true);
    setError(null);
    try {
      const next = await host.request();
      if (mounted.current && current === generation.current) {
        setStatus(next);
        setLoading(false);
      }
    } catch {
      if (mounted.current && current === generation.current) {
        setError(
          "macOS permissions could not be requested. Open System Settings to enable them, then refresh.",
        );
      }
    } finally {
      if (mounted.current) setRequesting(false);
    }
  }

  async function openSettings(pane: ComputerUsePermissionPane) {
    setOpening(pane);
    setError(null);
    try {
      await host.openSettings(pane);
    } catch {
      if (mounted.current) {
        setError(
          "System Settings could not be opened. Open Privacy & Security in System Settings and select the permission.",
        );
      }
    } finally {
      if (mounted.current) setOpening(null);
    }
  }

  const permissions = status?.status === "available" ? status : null;
  const missing =
    permissions && (!permissions.accessibility || !permissions.screenRecording);
  const busy = requesting || opening !== null;
  return (
    <SettingsSection
      title="Computer use on this Mac"
      description="Allow Tidebreak to read, capture, and control other apps. You still choose which apps each task may use."
    >
      {availability !== "local" ? (
        <p className="text-sm text-muted-foreground">
          {availability === "remote"
            ? "Switch to this Mac to set up its computer-use permissions. These settings do not grant access to a remote machine."
            : "Open Tidebreak on your Mac to set up computer use."}
        </p>
      ) : (
        <>
          {loading && !permissions && (
            <p role="status" className="text-sm text-muted-foreground">
              Checking macOS permissions…
            </p>
          )}
          {status?.status === "unsupported" && (
            <p className="text-sm text-muted-foreground">
              Native app control requires the macOS desktop app.
            </p>
          )}
          {permissions && (
            <>
              <p className="text-sm text-muted-foreground break-words">
                In System Settings, enable permissions for {permissions.appName}
                .
                {permissions.appIdentifier && (
                  <span className="mt-1 block break-all font-mono text-xs">
                    {permissions.appIdentifier}
                  </span>
                )}
              </p>
              <div className="divide-y divide-border">
                {(
                  [
                    [
                      "accessibility",
                      "Accessibility",
                      "Read app controls, click, and type.",
                      permissions.accessibility,
                    ],
                    [
                      "screen_recording",
                      "Screen Recording",
                      "Capture screenshots of apps and displays.",
                      permissions.screenRecording,
                    ],
                  ] as const
                ).map(([pane, label, description, granted]) => (
                  <div
                    key={pane}
                    className="flex flex-wrap items-center justify-between gap-3 py-3"
                  >
                    <div className="min-w-0 flex-1 basis-48">
                      <p className="text-sm font-medium">{label}</p>
                      <p className="text-sm text-muted-foreground">
                        {description}
                      </p>
                    </div>
                    <div className="flex items-center gap-3">
                      <Badge variant={granted ? "success" : "warning"}>
                        {granted ? "Allowed" : "Not allowed"}
                      </Badge>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={busy}
                        aria-label={`Open ${label} settings`}
                        onClick={() => void openSettings(pane)}
                      >
                        {opening === pane ? "Opening…" : "Open settings"}
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
              <p role="status" className="text-sm text-muted-foreground">
                {missing
                  ? "Enable the missing permissions, then return here. If macOS asks you to quit and reopen Tidebreak, do that before retrying the task."
                  : "macOS permissions are ready. Each task still asks for access to the apps it needs."}
              </p>
            </>
          )}
          {error && <SettingsError>{error}</SettingsError>}
          {status?.status !== "unsupported" && (
            <div className="flex flex-wrap gap-2">
              {missing && (
                <Button
                  size="sm"
                  disabled={busy || loading}
                  onClick={() => void request()}
                >
                  {requesting
                    ? "Requesting permissions…"
                    : "Request macOS permissions"}
                </Button>
              )}
              <Button
                variant="outline"
                size="sm"
                disabled={busy || loading}
                onClick={() => void refresh()}
              >
                {loading ? "Checking…" : "Refresh"}
              </Button>
            </div>
          )}
        </>
      )}
    </SettingsSection>
  );
}
