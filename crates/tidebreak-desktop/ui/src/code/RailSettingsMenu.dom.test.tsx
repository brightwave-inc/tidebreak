// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "./CodeUiStore";
import { RailSettingsMenu } from "./RailSettingsMenu";

afterEach(() => {
  cleanup();
  useCodeUiStore.setState({ railPrefs: DEFAULT_RAIL_PREFS });
  window.localStorage.clear();
});

describe("RailSettingsMenu", () => {
  it("keeps repository and status grouping in the settings popover", () => {
    render(<RailSettingsMenu />);
    expect(
      screen.queryByRole("radiogroup", { name: "Group workspaces" }),
    ).toBeNull();
    fireEvent.click(
      screen.getByRole("button", { name: "Workspace list settings" }),
    );
    const group = screen.getByRole("radiogroup", { name: "Group workspaces" });
    expect(
      within(group)
        .getAllByRole("radio")
        .map((radio) => radio.textContent),
    ).toEqual(["Repository", "Status"]);
    fireEvent.click(within(group).getByRole("radio", { name: "Status" }));
    expect(useCodeUiStore.getState().railPrefs.sortMode).toBe("by-status");
    expect(
      within(group).getByRole("radio", { name: "Status" }),
    ).toHaveAttribute("aria-checked", "true");
  });

  it("uses supplied preferences and actions without changing the shared store", () => {
    const onPrefsChange = vi.fn();
    const { rerender } = render(
      <RailSettingsMenu
        prefs={{ ...DEFAULT_RAIL_PREFS, sortMode: "by-status" }}
        onPrefsChange={onPrefsChange}
      />,
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Workspace list settings" }),
    );
    const group = screen.getByRole("radiogroup", { name: "Group workspaces" });
    expect(
      within(group).getByRole("radio", { name: "Status" }),
    ).toHaveAttribute("aria-checked", "true");
    fireEvent.click(within(group).getByRole("radio", { name: "Repository" }));
    expect(onPrefsChange).toHaveBeenCalledWith({ sortMode: "by-repo" });
    fireEvent.click(screen.getByRole("radio", { name: "Compact" }));
    expect(onPrefsChange).toHaveBeenCalledWith({ density: "compact" });
    expect(useCodeUiStore.getState().railPrefs).toEqual(DEFAULT_RAIL_PREFS);
    rerender(
      <RailSettingsMenu
        prefs={DEFAULT_RAIL_PREFS}
        onPrefsChange={onPrefsChange}
      />,
    );
    expect(
      within(group).getByRole("radio", { name: "Repository" }),
    ).toHaveAttribute("aria-checked", "true");
  });
});
