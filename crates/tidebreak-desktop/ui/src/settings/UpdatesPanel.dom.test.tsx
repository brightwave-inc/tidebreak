// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopUpdatePreferences, DesktopUpdateState } from "../updates";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  state: undefined as unknown,
  preferences: undefined as unknown,
  checked: undefined as unknown,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
  isTauri: () => true,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn(async () => "0.114.0"),
}));

import {
  downloadDesktopUpdate,
  useDesktopUpdatePreferences,
  useDesktopUpdates,
} from "../updates";
import { UpdatesPanel } from "./UpdatesPanel";

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

/** The Updates section, wired to the same hooks the app shell uses. */
function UpdatesSection() {
  const updates = useDesktopUpdates();
  const preferences = useDesktopUpdatePreferences();
  return (
    <UpdatesPanel
      state={updates.state}
      upToDate={updates.upToDate}
      preferences={preferences.preferences}
      preferencesSaving={preferences.saving}
      preferencesError={preferences.error}
      onCheck={updates.check}
      onDownload={downloadDesktopUpdate}
      onRestart={updates.restart}
      onAutomaticDownloadsChange={(enabled) =>
        void preferences.setAutomaticDownloads(enabled)
      }
    />
  );
}

beforeEach(() => {
  mocks.state = idle;
  mocks.preferences = automatic;
  mocks.checked = idle;
  mocks.invoke.mockImplementation(
    async (command: string, args?: { enabled: boolean }) => {
      switch (command) {
        case "desktop_update_state":
          return mocks.state;
        case "desktop_update_preferences":
          return mocks.preferences;
        case "check_for_update":
          return mocks.checked;
        case "set_automatic_update_downloads":
          return { automaticDownloads: args?.enabled, managed: false };
        default:
          return mocks.state;
      }
    },
  );
});

afterEach(cleanup);

describe("Updates section", () => {
  it("says you are up to date, with your version, after a check finds nothing", async () => {
    const user = userEvent.setup();
    render(<UpdatesSection />);

    await user.click(
      await screen.findByRole("button", { name: "Check for updates" }),
    );

    const result = await screen.findByText(
      "You're up to date · Tidebreak 0.114.0",
    );
    expect(result).toBeVisible();
    expect(result).toHaveAttribute("role", "status");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(mocks.invoke).toHaveBeenCalledWith("check_for_update");
  });

  it("keeps the result until the next check starts", async () => {
    const user = userEvent.setup();
    render(<UpdatesSection />);
    await user.click(
      await screen.findByRole("button", { name: "Check for updates" }),
    );
    await screen.findByText("You're up to date · Tidebreak 0.114.0");

    mocks.checked = new Promise(() => {});
    await user.click(screen.getByRole("button", { name: "Check for updates" }));

    expect(screen.getByRole("button", { name: "Checking…" })).toBeDisabled();
    expect(screen.queryByText(/You're up to date/)).toBeNull();
  });

  it("says why a check failed", async () => {
    const user = userEvent.setup();
    const reason =
      "Could not check for updates. Tidebreak could not reach the update server. Check your internet connection and try again.";
    mocks.checked = { ...idle, error: reason };
    render(<UpdatesSection />);

    await user.click(
      await screen.findByRole("button", { name: "Check for updates" }),
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(reason);
    expect(screen.queryByText(/You're up to date/)).toBeNull();
  });

  it("saves the automatic-download setting when you change it", async () => {
    const user = userEvent.setup();
    render(<UpdatesSection />);
    const toggle = await screen.findByRole("switch", {
      name: "Download updates automatically",
    });
    await waitFor(() => expect(toggle).toBeEnabled());
    expect(toggle).toBeChecked();

    await user.click(toggle);

    expect(mocks.invoke).toHaveBeenCalledWith(
      "set_automatic_update_downloads",
      { enabled: false },
    );
    await waitFor(() => expect(toggle).not.toBeChecked());
  });

  it("puts the switch back and says so when the setting cannot be saved", async () => {
    const user = userEvent.setup();
    const saveFailed = "Could not save the setting. Try again.";
    const invokeDefault = mocks.invoke.getMockImplementation();
    mocks.invoke.mockImplementation(
      async (command: string, args?: { enabled: boolean }) => {
        if (command === "set_automatic_update_downloads") throw saveFailed;
        return invokeDefault?.(command, args);
      },
    );
    render(<UpdatesSection />);
    const toggle = await screen.findByRole("switch", {
      name: "Download updates automatically",
    });
    await waitFor(() => expect(toggle).toBeEnabled());

    await user.click(toggle);

    expect(await screen.findByRole("alert")).toHaveTextContent(saveFailed);
    expect(toggle).toBeChecked();
  });

  it("shows a setting your organization manages as locked", async () => {
    mocks.preferences = { automaticDownloads: false, managed: true };
    render(<UpdatesSection />);

    expect(
      await screen.findByText(
        "Managed by your organization. Tidebreak tells you when an update is available and downloads it only when you ask.",
      ),
    ).toBeVisible();
    const toggle = screen.getByRole("switch", {
      name: "Download updates automatically",
    });
    expect(toggle).toBeDisabled();
    expect(toggle).not.toBeChecked();
  });

  it("downloads an available update only when you ask", async () => {
    const user = userEvent.setup();
    mocks.state = { ...idle, status: "available", version: "0.115.0" };
    mocks.preferences = { automaticDownloads: false, managed: false };
    render(<UpdatesSection />);

    expect(
      await screen.findByText("Version 0.115.0 is available."),
    ).toBeVisible();
    expect(mocks.invoke).not.toHaveBeenCalledWith("download_update");

    await user.click(screen.getByRole("button", { name: "Download update" }));

    expect(mocks.invoke).toHaveBeenCalledWith("download_update");
  });
});
