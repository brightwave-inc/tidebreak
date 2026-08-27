// @vitest-environment jsdom
import { describe, expect, it } from "vitest";

import {
  defaultSettingsPathFor,
  providersSearch,
  settingsSectionsFor,
  SETTINGS_SECTIONS,
} from "./sections";

describe("settings sections", () => {
  it("opens a managed profile on the gateway and drops the providers rail entry", () => {
    const managed = settingsSectionsFor(true);
    expect(managed.map((section) => section.path)).not.toContain("providers");
    expect(defaultSettingsPathFor(true)).toBe("/settings/gateway");
  });

  it("keeps the gateway entry on an unmanaged profile's rail", () => {
    // Policy is still the only gateway source, so an unmanaged profile has no
    // gateway to configure — but the section is also where a machine is
    // attached, and a machine behind no gateway is reachable with its own
    // token. Hiding it would leave that machine with no route in the app.
    const unmanaged = settingsSectionsFor(false);
    expect(unmanaged.map((section) => section.path)).toContain("gateway");
    expect(defaultSettingsPathFor(false)).toBe("/settings/providers");
  });

  it("has no machine section of its own", () => {
    // Folded into Model Gateway: the gateway that governs a profile is the
    // one that hosts the machine it offers, so the two are one page.
    expect(SETTINGS_SECTIONS.map((section) => section.path)).not.toContain(
      "machine",
    );
  });

  it("always exposes coding harnesses and has no experimental section", () => {
    for (const managed of [false, true]) {
      const paths = settingsSectionsFor(managed).map((section) => section.path);
      expect(paths).toContain("coding-harnesses");
      expect(paths).not.toContain("experimental");
    }
  });

  it("always exposes quick-action prompts", () => {
    for (const managed of [false, true]) {
      expect(
        settingsSectionsFor(managed).map((section) => section.path),
      ).toContain("quick-actions");
    }
  });

  it("keeps a providers deep link to what it can act on", () => {
    // The picker's CTAs address this route; anything else in the URL — a stale
    // link, a hand-edited one — must reach the panel as nothing rather than as
    // an instruction it half-understands.
    expect(
      providersSearch({ provider: "anthropic", focus: "credential" }),
    ).toEqual({ provider: "anthropic", focus: "credential" });
    expect(providersSearch({ provider: 7, focus: "everything" })).toEqual({
      provider: undefined,
      focus: undefined,
    });
  });
});
