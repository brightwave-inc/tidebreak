import { useEffect, useState } from "react";
import { RefreshCw, RotateCw } from "lucide-react";
import { Button } from "@/components/ui/button";
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
      return "OpenWave checks for signed updates automatically.";
  }
}

export function UpdatesPanel({
  state,
  onCheck,
  onRestart,
}: {
  state: DesktopUpdateState;
  onCheck: () => Promise<DesktopUpdateState>;
  onRestart: () => Promise<void>;
}) {
  const [manualResult, setManualResult] = useState<string | null>(null);
  const busy = state.status === "checking" || state.status === "downloading";

  useEffect(() => {
    if (busy || state.status === "ready" || state.error) {
      setManualResult(null);
    }
  }, [busy, state.error, state.status]);

  async function check() {
    setManualResult(null);
    const next = await onCheck();
    if (next.enabled && next.status === "idle" && !next.error) {
      setManualResult("OpenWave is up to date.");
    }
  }

  return (
    <SettingsPanel
      title="Updates"
      description="OpenWave checks shortly after launch and every five minutes. Updates are downloaded in the background, but OpenWave only restarts when you choose."
      busy={busy}
    >
      <SettingsSection title="Automatic updates">
        <p className="text-sm text-muted-foreground" aria-live="polite">
          {updateStateSummary(state)}
        </p>
        {manualResult && (
          <p className="text-sm text-muted-foreground" role="status">
            {manualResult}
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
              onClick={() => void check()}
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
    </SettingsPanel>
  );
}
