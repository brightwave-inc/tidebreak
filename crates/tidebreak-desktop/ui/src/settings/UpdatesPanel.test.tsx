import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { DesktopUpdateState } from "../updates";
import { UpdatesPanel, updateStateSummary } from "./UpdatesPanel";

const idle: DesktopUpdateState = {
  status: "idle",
  version: null,
  error: null,
  enabled: true,
};

describe("UpdatesPanel", () => {
  it("keeps update checks disabled outside supported packaged builds", () => {
    const markup = renderToStaticMarkup(
      <UpdatesPanel
        state={{ ...idle, enabled: false }}
        onCheck={vi.fn()}
        onRestart={vi.fn()}
      />,
    );

    expect(markup).toContain("available in packaged release builds");
    expect(markup).toContain("disabled");
  });

  it("offers an explicit relaunch only after an update is staged", () => {
    const state: DesktopUpdateState = {
      ...idle,
      status: "ready",
      version: "1.2.3",
    };
    const markup = renderToStaticMarkup(
      <UpdatesPanel
        state={state}
        onCheck={vi.fn()}
        onRestart={vi.fn()}
      />,
    );

    expect(updateStateSummary(state)).toContain("Version 1.2.3");
    expect(markup).toContain("Restart to update");
    expect(markup).toContain(
      "installs only after you choose Restart to update",
    );
    expect(markup).not.toContain("installs on its own");
    expect(markup).not.toContain("Check for updates");
  });

  it("shows generic host errors without exposing updater diagnostics", () => {
    const markup = renderToStaticMarkup(
      <UpdatesPanel
        state={{ ...idle, error: "Could not check for updates. Try again later." }}
        onCheck={vi.fn()}
        onRestart={vi.fn()}
      />,
    );

    expect(markup).toContain("Could not check for updates");
    expect(markup).toContain('role="alert"');
  });
});
