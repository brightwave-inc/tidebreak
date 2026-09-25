import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import {
  CircleCheck,
  Download,
  LifeBuoy,
  RefreshCw,
  RotateCw,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { ClipboardCopyButton } from "../ClipboardCopyButton";
import { hasNativeHost } from "../host";
import type { DesktopUpdatePreferences, DesktopUpdateState } from "../updates";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";

export function updateStateSummary(state: DesktopUpdateState): string {
  if (!state.enabled) {
    return "Automatic updates are available in packaged release builds.";
  }
  switch (state.status) {
    case "checking":
      return "Checking for updates…";
    case "available":
      return state.version
        ? `Version ${state.version} is available.`
        : "An update is available.";
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

/** The result a check that found nothing newer leaves on screen. */
export function upToDateMessage(version: string | null): string {
  return version
    ? `You're up to date · Tidebreak ${version}`
    : "You're up to date.";
}

/**
 * The hint under the automatic-download switch. A managed setting also says
 * what your organization chose, because a disabled switch is easy to misread.
 */
export function automaticDownloadsHint(
  preferences: DesktopUpdatePreferences | null,
): string {
  if (preferences === null) {
    return "Loading this setting…";
  }
  if (!preferences.managed) {
    return "Tidebreak downloads new versions in the background. When this is off, Tidebreak tells you when an update is available and downloads it only when you ask.";
  }
  return preferences.automaticDownloads
    ? "Managed by your organization. Tidebreak downloads new versions in the background."
    : "Managed by your organization. Tidebreak tells you when an update is available and downloads it only when you ask.";
}

export function UpdatesPanel({
  state,
  upToDate = false,
  appVersion,
  preferences,
  preferencesSaving = false,
  preferencesError = null,
  onCheck,
  onDownload,
  onRestart,
  onAutomaticDownloadsChange,
  onReportProblem,
}: {
  state: DesktopUpdateState;
  /** The most recent explicit check confirmed the app is current. */
  upToDate?: boolean;
  /** The running version. When omitted, the panel asks the desktop app. */
  appVersion?: string | null;
  /** The automatic-download setting, or `null` until the desktop reports it. */
  preferences: DesktopUpdatePreferences | null;
  preferencesSaving?: boolean;
  preferencesError?: string | null;
  onCheck: () => Promise<DesktopUpdateState>;
  onDownload: () => Promise<unknown>;
  onRestart: () => Promise<void>;
  onAutomaticDownloadsChange: (enabled: boolean) => void;
  /**
   * Open Report a problem. It lives here as well as in the Help menu because
   * Windows and Linux builds have no menu bar.
   */
  onReportProblem?: () => void;
}) {
  const [reportedVersion, setReportedVersion] = useState<string | null>(null);
  const version = appVersion ?? reportedVersion;
  const busy = state.status === "checking" || state.status === "downloading";
  const managed = preferences?.managed ?? false;

  // Only the packaged desktop host can report its version; a browser dev build
  // has none, so the line falls back to a plain note there.
  useEffect(() => {
    if (appVersion !== undefined || !hasNativeHost()) return;
    let cancelled = false;
    void getVersion()
      .then((value) => {
        if (!cancelled) setReportedVersion(value);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [appVersion]);

  return (
    <SettingsPanel
      title="Updates"
      description="Tidebreak checks for updates shortly after launch and every hour. An update installs only after you choose Restart to update."
      busy={busy}
    >
      <SettingsSection title="Automatic updates">
        <p className="text-sm text-muted-foreground" aria-live="polite">
          {updateStateSummary(state)}
        </p>
        {upToDate && (
          <p
            className="flex items-start gap-2 text-sm text-foreground"
            role="status"
          >
            <CircleCheck
              className="mt-0.5 size-3.5 shrink-0 text-success"
              aria-hidden="true"
            />
            {upToDateMessage(version)}
          </p>
        )}
        {state.error && <SettingsError>{state.error}</SettingsError>}
        <div>
          {state.status === "ready" ? (
            <Button type="button" onClick={() => void onRestart()}>
              <RotateCw />
              Restart to update
            </Button>
          ) : state.status === "available" ? (
            <Button type="button" onClick={() => void onDownload()}>
              <Download />
              Download update
            </Button>
          ) : (
            <Button
              type="button"
              variant="outline"
              disabled={!state.enabled || busy}
              onClick={() => void onCheck()}
            >
              {busy ? <Spinner aria-hidden /> : <RefreshCw />}
              {state.status === "checking"
                ? "Checking…"
                : state.status === "downloading"
                  ? "Downloading…"
                  : "Check for updates"}
            </Button>
          )}
        </div>
        <SettingsField
          label="Download updates automatically"
          hint={automaticDownloadsHint(preferences)}
        >
          <Switch
            checked={preferences?.automaticDownloads ?? true}
            disabled={
              !state.enabled ||
              preferences === null ||
              managed ||
              preferencesSaving
            }
            onCheckedChange={onAutomaticDownloadsChange}
            aria-label="Download updates automatically"
          />
        </SettingsField>
        {preferencesError && <SettingsError>{preferencesError}</SettingsError>}
      </SettingsSection>

      {onReportProblem && (
        <SettingsSection
          title="Report a problem"
          description="Save a diagnostics report and open a GitHub issue with your version, operating system, and architecture filled in."
        >
          <div>
            <Button type="button" variant="outline" onClick={onReportProblem}>
              <LifeBuoy />
              Report a problem…
            </Button>
          </div>
        </SettingsSection>
      )}

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
