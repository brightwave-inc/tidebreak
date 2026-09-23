import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { DesktopUpdatePreferences, DesktopUpdateState } from "../updates";
import { UpdatesPanel, updateStateSummary } from "./UpdatesPanel";

const idle: DesktopUpdateState = {
  status: "idle",
  version: null,
  error: null,
  enabled: true,
};

const automatic: DesktopUpdatePreferences = {
  automaticDownloads: true,
  managed: false,
};

function panel(
  state: DesktopUpdateState,
  preferences: DesktopUpdatePreferences | null = automatic,
) {
  return renderToStaticMarkup(
    <UpdatesPanel
      state={state}
      appVersion="0.114.0"
      preferences={preferences}
      onCheck={vi.fn()}
      onDownload={vi.fn()}
      onRestart={vi.fn()}
      onAutomaticDownloadsChange={vi.fn()}
    />,
  );
}

describe("UpdatesPanel", () => {
  it("does not warn that an update may wipe local data", () => {
    // Updates keep local data: the database upgrades in place, and a copy
    // is saved first.
    expect(panel(idle)).not.toContain("wipe");
  });

  it("keeps update checks disabled outside supported packaged builds", () => {
    const markup = panel({ ...idle, enabled: false });

    expect(markup).toContain("available in packaged release builds");
    expect(markup).toContain("disabled");
  });

  it("offers an explicit relaunch only after an update is staged", () => {
    const state: DesktopUpdateState = {
      ...idle,
      status: "ready",
      version: "1.2.3",
    };
    const markup = panel(state);

    expect(updateStateSummary(state)).toContain("Version 1.2.3");
    expect(markup).toContain("Restart to update");
    expect(markup).toContain(
      "installs only after you choose Restart to update",
    );
    expect(markup).not.toContain("installs on its own");
    expect(markup).not.toContain("Check for updates");
  });

  it("offers the download when an update waits to be downloaded", () => {
    const state: DesktopUpdateState = {
      ...idle,
      status: "available",
      version: "1.2.3",
    };
    const markup = panel(state, { automaticDownloads: false, managed: false });

    expect(updateStateSummary(state)).toBe("Version 1.2.3 is available.");
    expect(markup).toContain("Download update");
    expect(markup).not.toContain("Restart to update</button>");
    expect(markup).toContain("downloads it only when you ask");
  });

  it("holds the switch until the desktop reports the setting", () => {
    const markup = panel(idle, null);

    expect(markup).toContain("Loading this setting…");
    expect(markup).toMatch(/role="switch"[^>]*disabled/);
  });

  it("shows generic host errors without exposing updater diagnostics", () => {
    const markup = panel({
      ...idle,
      error: "Could not check for updates. Try again later.",
    });

    expect(markup).toContain("Could not check for updates");
    expect(markup).toContain('role="alert"');
  });
});
