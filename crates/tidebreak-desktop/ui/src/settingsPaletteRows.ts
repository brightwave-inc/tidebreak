import { settingsSectionsFor } from "./settings/sections";
import type { PaletteRow } from "./CommandPalette";

/**
 * Every settings section as its own palette row.
 *
 * Settings is a rail of a dozen panels, and reaching one has always meant
 * opening settings and then finding it. Naming each section here makes the
 * panel the target instead of the trip: "appear" lands on Appearance without
 * the reader deciding which section a thing lives in first.
 *
 * The list is the same one the settings rail draws, filtered the same way, so
 * a managed profile does not get offered a section it cannot open. `navigate`
 * is a callback because settings sections come from a runtime table and never
 * enter the router's generated path union.
 */
export function settingsPaletteRows(input: {
  managed: boolean;
  navigate: (path: string) => void;
}): PaletteRow[] {
  return settingsSectionsFor(input.managed).map((section) => ({
    id: `settings:${section.path}`,
    section: "settings",
    label: section.label,
    // So that typing the word finds every section, not only the ones whose
    // label happens to contain it.
    keywords: `settings ${section.path.replace(/-/g, " ")}`,
    icon: section.icon,
    onSelect: () => input.navigate(`/settings/${section.path}`),
  }));
}
