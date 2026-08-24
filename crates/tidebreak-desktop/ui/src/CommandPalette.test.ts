import { describe, expect, it } from "vitest";

import {
  PALETTE_SECTION_ORDER,
  parsePaletteQuery,
  rankPaletteRows,
  rememberPaletteRow,
  type PaletteRow,
  type PaletteSection,
} from "./CommandPalette";

function row(
  id: string,
  section: PaletteSection,
  label: string,
  keywords?: string,
): PaletteRow {
  return { id, section, label, keywords, onSelect: () => {} };
}

const ROWS: PaletteRow[] = [
  row("suggested:push", "suggested", "Push"),
  row("workspace:a", "workspaces", "Pluggable memory system", "feat/memory"),
  row("workspace:b", "workspaces", "Pr state sync lag", "fix/pr-state"),
  row("action:rename", "actions", "Rename"),
  row("action:terminal", "actions", "Toggle terminal"),
  row("ship:merge", "ship", "Merge, or auto-merge once the checks pass"),
  row("settings:models", "settings", "Models", "settings"),
  row("settings:appearance", "settings", "Appearance", "settings"),
  row("navigate:runs", "navigate", "Runs"),
];

describe("palette query", () => {
  it("reads a leading prefix as a scope and strips it from the search", () => {
    expect(parsePaletteQuery(">merge")).toMatchObject({
      prefix: ">",
      scopeLabel: "commands",
      query: "merge",
    });
    expect(parsePaletteQuery("@ mem")).toMatchObject({
      prefix: "@",
      query: "mem",
    });
  });

  it("leaves a prefix character in the middle of a query alone", () => {
    // Issue numbers and colors both carry one. Treating it as a scope would
    // make the list jump as the reader typed.
    const parsed = parsePaletteQuery("fix #2519");
    expect(parsed.prefix).toBeNull();
    expect(parsed.query).toBe("fix #2519");
  });
});

describe("palette ranking", () => {
  it("groups into the order the sections are listed in", () => {
    const groups = rankPaletteRows(ROWS, "");
    const seen = groups.map((group) => group.section);
    const expected = PALETTE_SECTION_ORDER.filter((section) =>
      seen.includes(section),
    );
    expect(seen).toEqual(expected);
    expect(seen[0]).toBe("suggested");
  });

  it("matches labels and keywords, so a branch finds its workspace", () => {
    const groups = rankPaletteRows(ROWS, "memory");
    const workspaces = groups.find((group) => group.section === "workspaces");
    expect(workspaces?.rows.map((entry) => entry.id)).toEqual(["workspace:a"]);

    const byBranch = rankPaletteRows(ROWS, "pr-state");
    expect(
      byBranch.find((group) => group.section === "workspaces")?.rows[0]?.id,
    ).toBe("workspace:b");
  });

  it("shows one suggestion, never a list of them", () => {
    const many = [
      ...ROWS,
      row("suggested:merge", "suggested", "Merge"),
      row("suggested:watch", "suggested", "Watch"),
    ];
    const groups = rankPaletteRows(many, "");
    expect(groups[0]?.rows).toHaveLength(1);
  });

  it("caps a section until a prefix says it is the only one being read", () => {
    const many = [
      ...ROWS,
      ...Array.from({ length: 10 }, (_unused, index) =>
        row(`workspace:extra-${index}`, "workspaces", `Extra ${index}`),
      ),
    ];
    const mixed = rankPaletteRows(many, "");
    expect(
      mixed.find((group) => group.section === "workspaces")?.rows,
    ).toHaveLength(5);

    const scoped = rankPaletteRows(many, "@");
    expect(scoped.every((group) => group.section === "workspaces")).toBe(true);
    expect(scoped[0]?.rows.length).toBeGreaterThan(5);
  });

  it("drops every section a prefix did not ask for", () => {
    const groups = rankPaletteRows(ROWS, ">");
    expect(groups.map((group) => group.section)).toEqual(["actions", "ship"]);
  });

  it("floats a recently picked row when nothing is typed", () => {
    const groups = rankPaletteRows(ROWS, "", {
      recents: ["action:terminal"],
    });
    const actions = groups.find((group) => group.section === "actions");
    expect(actions?.rows[0]?.id).toBe("action:terminal");
  });

  it("lets a real text match beat the memory", () => {
    // A reader who types is describing what they want, not asking for their
    // history.
    const groups = rankPaletteRows(ROWS, "rename", {
      recents: ["action:terminal"],
    });
    const actions = groups.find((group) => group.section === "actions");
    expect(actions?.rows[0]?.id).toBe("action:rename");
  });

  it("finds every settings section under the word settings", () => {
    const groups = rankPaletteRows(ROWS, "settings");
    const settings = groups.find((group) => group.section === "settings");
    expect(settings?.rows.map((entry) => entry.id)).toEqual([
      "settings:models",
      "settings:appearance",
    ]);
  });
});

describe("palette recents", () => {
  it("moves a repeat pick to the front rather than duplicating it", () => {
    const once = rememberPaletteRow("a", []);
    const twice = rememberPaletteRow("b", once);
    expect(twice).toEqual(["b", "a"]);
    expect(rememberPaletteRow("a", twice)).toEqual(["a", "b"]);
  });
});
