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

    // An unmanaged profile is untouched: every section, providers first.
    expect(settingsSectionsFor(false)).toEqual(SETTINGS_SECTIONS);
    expect(defaultSettingsPathFor(false)).toBe("/settings/providers");
  });
});
