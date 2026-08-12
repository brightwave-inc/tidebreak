import { describe, expect, it } from "vitest";

import {
  availableSlashOptions,
  skillsToInvoke,
  slashOptionsFromCatalog,
  MAX_INVOKED_SKILLS,
  type SlashOption,
} from "./ComposerSlash";
import type { PluginCatalog } from "./api";

const CATALOG: PluginCatalog = {
  plugins: [
    {
      name: "documents",
      display_name: "Documents",
      description: "Writes Word, Excel, and PowerPoint files.",
      category: "documents",
      origin: "builtin",
      capabilities: [],
      compatibility: { status: "unchecked", issues: [] },
      enabled: true,
      skills: [
        { name: "docx", description: "Documents.", origin: "builtin", enabled: true },
        { name: "pptx", description: "Decks.", origin: "builtin", enabled: false },
      ],
    },
    {
      name: "charts",
      display_name: "Charts",
      description: "Plots data.",
      category: "visualization",
      origin: "builtin",
      capabilities: [],
      compatibility: { status: "unchecked", issues: [] },
      enabled: false,
      skills: [
        { name: "charts", description: "Plots.", origin: "builtin", enabled: true },
      ],
    },
  ],
  skills: [
    { name: "notes", description: "Takes notes.", origin: "user", enabled: true },
  ],
  prompts: [
    {
      name: "weekly-update",
      description: "The Monday note.",
      origin: "user",
      plugin: null,
      enabled: true,
    },
  ],
};

describe("slashOptionsFromCatalog", () => {
  it("offers a bundle for its enabled members, and members only once the bundle is on", () => {
    const options = slashOptionsFromCatalog(CATALOG);
    expect(
      options.map((option) => [option.kind, option.name, option.skills]),
    ).toEqual([
      ["plugin", "documents", ["docx"]],
      ["plugin", "charts", ["charts"]],
      // `pptx` is off on its own, and `charts` is inside a bundle that is off.
      ["skill", "docx", undefined],
      ["skill", "notes", undefined],
      ["prompt", "weekly-update", undefined],
    ]);
    // A member skill wears its bundle's category, which is what its chip draws.
    expect(options.find((option) => option.name === "docx")?.category).toBe(
      "documents",
    );
  });
});

describe("skillsToInvoke", () => {
  const bundle: SlashOption = {
    kind: "plugin",
    name: "documents",
    label: "Documents",
    description: "",
    skills: ["docx", "pptx", "xlsx"],
  };

  it("stands for its members, skipping what the message already carries", () => {
    expect(skillsToInvoke(bundle, ["pptx"])).toEqual(["docx", "xlsx"]);
  });

  it("fills the room the cap leaves rather than refusing the pick", () => {
    const taken = Array.from(
      { length: MAX_INVOKED_SKILLS - 1 },
      (_, index) => `taken-${index}`,
    );
    expect(skillsToInvoke(bundle, taken)).toEqual(["docx"]);
    expect(skillsToInvoke(bundle, [...taken, "one-more"])).toEqual([]);
  });
});

describe("availableSlashOptions", () => {
  const options = slashOptionsFromCatalog(CATALOG);

  it("keeps invocations reachable while a turn runs, marking disabled bundles", () => {
    // A steer carries its own invocation, so skills stay pickable. The `charts`
    // bundle is off, and picking it mid-turn would flip it on install-wide and
    // name a manifest this turn never staged — so its row stays, unpickable,
    // rather than vanishing from a library the reader saw a moment ago.
    const available = availableSlashOptions(options, [], { steering: true });
    expect(available.map((o) => o.name)).toEqual([
      "documents",
      "charts",
      "docx",
      "notes",
      "weekly-update",
    ]);
    expect(
      available.filter((o) => o.unavailable).map((o) => o.name),
    ).toEqual(["charts"]);
  });

  it("drops a bundle only once every member it could add is on the message", () => {
    expect(
      availableSlashOptions(options, ["docx"], { steering: false }).map(
        (option) => option.name,
      ),
    ).toEqual(["charts", "notes", "weekly-update"]);
  });
});
