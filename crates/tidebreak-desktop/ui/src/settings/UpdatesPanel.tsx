import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { RefreshCw, RotateCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ClipboardCopyButton } from "../ClipboardCopyButton";
import { hasNativeHost } from "../host";
import type { DesktopUpdateState } from "../updates";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";

export function updateStateSummary(state: DesktopUpdateState): string {
  if (!state.enabled) {
    return "Automatic updates are available in packaged macOS builds.";
  }
  switch (state.status) {
    case "checking":
      return "Checking for updates…";
    case "downloading":
      return state.version
        ? `Downloading and verifying version ${state.version}…`
        : "Downloading and verifying the update…";
    case "ready":
      return state.version
        ? `Version ${state.version} is ready to install.`
        : "An update is ready to install.";
    case "idle":
      return "Tidebreak checks for signed updates automatically.";
  }
}

export function UpdatesPanel({
  state,
  upToDate = false,
  onCheck,
  onRestart,
}: {
  state: DesktopUpdateState;
  /** The most recent explicit check confirmed the app is current. */
  upToDate?: boolean;
  onCheck: () => Promise<DesktopUpdateState>;
  onRestart: () => Promise<void>;
}) {
  const [version, setVersion] = useState<string | null>(null);
  const busy = state.status === "checking" || state.status === "downloading";

  // Only the packaged desktop host can report its version; a browser dev build
  // has none, so the line falls back to a plain note there.
  useEffect(() => {
    if (!hasNativeHost()) return;
    let cancelled = false;
    void getVersion()
      .then((value) => {
        if (!cancelled) setVersion(value);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);


  return (
    <SettingsPanel
      title="Updates"
      description="Tidebreak checks shortly after launch and every five minutes, and downloads updates in the background. A downloaded update installs only after you choose Restart to update."
      busy={busy}
    >
      <SettingsSection title="Automatic updates">
        <p className="text-sm text-muted-foreground" aria-live="polite">
          {updateStateSummary(state)}
        </p>
        {upToDate && (
          <p className="text-sm text-muted-foreground" role="status">
            Tidebreak is up to date.
          </p>
        )}
        {state.error && <SettingsError>{state.error}</SettingsError>}
        <div>
          {state.status === "ready" ? (
            <Button type="button" onClick={() => void onRestart()}>
              <RotateCw />
              Restart to update
            </Button>
          ) : (
            <Button
              type="button"
              variant="outline"
              disabled={!state.enabled || busy}
              onClick={() => void onCheck()}
            >
              <RefreshCw className={busy ? "animate-spin" : undefined} />
              {state.status === "checking"
                ? "Checking…"
                : state.status === "downloading"
                  ? "Downloading…"
                  : "Check for updates"}
            </Button>
          )}
        </div>
      </SettingsSection>

      <SettingsSection title="About">
        <div className="flex items-center justify-between gap-4">
          <p className="text-sm text-muted-foreground">
            {version
              ? `Tidebreak ${version}`
              : "Version is reported by the desktop app."}
          </p>
          {version && (
            <ClipboardCopyButton
              value={version}
              label="Copy version"
              copiedAnnouncement="Version copied to clipboard."
              failedAnnouncement="Version could not be copied."
              className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-hidden"
            />
          )}
        </div>
      </SettingsSection>
    </SettingsPanel>
  );
}
