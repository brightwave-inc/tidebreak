// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import {
  finishedNotificationsEnabled,
  needsYouNotificationsEnabled,
} from "@/NotificationPreferences";
import { NotificationsPanel } from "./NotificationsPanel";
import { SETTINGS_SECTIONS } from "./sections";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

describe("Notifications settings", () => {
  it("saves each switch the moment it changes, separately", async () => {
    const user = userEvent.setup();
    render(<NotificationsPanel />);

    const needsYou = screen.getByRole("switch", {
      name: "When an agent needs you",
    });
    const finished = screen.getByRole("switch", {
      name: "When an agent finishes",
    });
    expect(needsYou).toHaveAttribute("data-state", "checked");
    expect(finished).toHaveAttribute("data-state", "checked");

    await user.click(needsYou);
    expect(needsYouNotificationsEnabled()).toBe(false);
    expect(finishedNotificationsEnabled()).toBe(true);

    await user.click(finished);
    expect(finishedNotificationsEnabled()).toBe(false);

    await user.click(needsYou);
    expect(needsYouNotificationsEnabled()).toBe(true);
  });

  it("keeps an earlier choice to turn off finished notifications", () => {
    // Before this section, one switch in Appearance covered finished work
    // under this key. Turning it off there still means off here.
    window.localStorage.setItem("tidebreak.desktop-notifications", "off");
    render(<NotificationsPanel />);

    expect(
      screen.getByRole("switch", { name: "When an agent finishes" }),
    ).toHaveAttribute("data-state", "unchecked");
    expect(
      screen.getByRole("switch", { name: "When an agent needs you" }),
    ).toHaveAttribute("data-state", "checked");
  });

  it("is its own section that search can find", () => {
    const section = SETTINGS_SECTIONS.find(
      (candidate) => candidate.path === "notifications",
    );
    expect(section?.label).toBe("Notifications");
    expect(section?.keywords).toMatch(/dock/);
    expect(
      SETTINGS_SECTIONS.find((candidate) => candidate.path === "appearance")
        ?.keywords,
    ).not.toMatch(/notifications/);
  });
});
