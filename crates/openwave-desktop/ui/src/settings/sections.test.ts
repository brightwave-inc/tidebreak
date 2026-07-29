// @vitest-environment jsdom
import { describe, expect, it } from "vitest";

import {
  defaultSettingsPathFor,
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
});
