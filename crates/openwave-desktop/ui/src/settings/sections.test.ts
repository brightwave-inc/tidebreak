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

  it("drops the gateway entry from an unmanaged profile's rail", () => {
    // Policy is the only gateway source: an unmanaged profile has nothing to
    // configure under Model Gateway, so the section leaves its rail — while
    // every route in the registry still resolves for deep links.
    const unmanaged = settingsSectionsFor(false);
    expect(unmanaged.map((section) => section.path)).not.toContain("gateway");
    expect(defaultSettingsPathFor(false)).toBe("/settings/providers");
    expect(SETTINGS_SECTIONS.map((section) => section.path)).toContain(
      "gateway",
    );
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
