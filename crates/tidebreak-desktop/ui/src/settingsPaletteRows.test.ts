// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";

import { rankPaletteRows } from "./CommandPalette";
import { settingsPaletteRows } from "./settingsPaletteRows";

function firstSettingsMatch(query: string, managed = false): string | null {
  const rows = settingsPaletteRows({ managed, navigate: vi.fn() });
  const group = rankPaletteRows(rows, query).find(
    (entry) => entry.section === "settings",
  );
  return group?.rows[0]?.id ?? null;
}

describe("settings in the command palette", () => {
  it("finds a section by what it holds, not only by its name", () => {
    // Each of these found nothing when rows matched only "settings" and the
    // section's path, because none of the words is in a label.
    const expected: Array<[string, string]> = [
      ["mcp", "connected-apps"],
      ["servers", "connected-apps"],
      ["tools", "connected-apps"],
      ["theme", "appearance"],
      ["dark", "appearance"],
      ["api key", "providers"],
      ["key", "providers"],
      ["ollama", "providers"],
      ["sandbox", "code-execution"],
      ["docker", "code-execution"],
      ["slack", "channels"],
      ["voice", "voice-transcription"],
    ];
    for (const [query, section] of expected) {
      expect(firstSettingsMatch(query), query).toBe(`settings:${section}`);
    }
  });

  it("still finds a section by its own name first", () => {
    expect(firstSettingsMatch("models")).toBe("settings:models");
    expect(firstSettingsMatch("memory")).toBe("settings:memory");
  });

  it("offers a managed profile only the sections it can open", () => {
    // Providers is off a managed profile's rail, so its words find nothing
    // there rather than a section the reader cannot use.
    expect(firstSettingsMatch("ollama", true)).toBeNull();
  });
});
