import { describe, expect, it } from "vitest";

import {
  parseSlashCommand,
  withSlashCommands,
  SLASH_COMMANDS,
  type SlashCommand,
} from "./ComposerCommands";
import { filterSlashOptions, type SlashOption } from "./ComposerSlash";

const SKILL: SlashOption = {
  kind: "skill",
  name: "usage",
  label: "Usage",
  description: "A skill that happens to be called usage.",
};

const COMMANDS: readonly SlashCommand[] = [
  { name: "usage", description: "Show usage.", takesArgument: false },
];

describe("withSlashCommands", () => {
  it("reserves command names and offers them ahead of the library", () => {
    const options = withSlashCommands([SKILL], COMMANDS);
    expect(options.map((option) => [option.kind, option.name])).toEqual([
      ["command", "usage"],
    ]);
    // And the reserved name still wins once the query narrows the list.
    expect(filterSlashOptions(options, "usa")[0]?.kind).toBe("command");
  });
});

describe("parseSlashCommand", () => {
  it("claims a line that names a command, and leaves prose alone", () => {
    expect(parseSlashCommand("/usage", COMMANDS)?.command.name).toBe("usage");
    expect(parseSlashCommand("  /usage  ", COMMANDS)?.argument).toBe("");
    expect(parseSlashCommand("what does /usage show?", COMMANDS)).toBeNull();
    expect(parseSlashCommand("/usagex", COMMANDS)).toBeNull();
    expect(parseSlashCommand("/unknown", COMMANDS)).toBeNull();
    // A command that takes no argument is not one when a sentence follows it.
    expect(parseSlashCommand("/usage of the new API", COMMANDS)).toBeNull();
  });

  it("gives a command that takes one the rest of the line", () => {
    // `/compact` is the shipped case: everything after the name is what the
    // summary should keep.
    expect(
      parseSlashCommand("/compact  keep the API design "),
    ).toEqual({
      command: SLASH_COMMANDS.find((command) => command.name === "compact"),
      argument: "keep the API design",
    });
    const commands: readonly SlashCommand[] = [
      { name: "usage", description: "", takesArgument: true },
    ];
    expect(parseSlashCommand("/usage  keep the API design ", commands)).toEqual({
      command: commands[0],
      argument: "keep the API design",
    });
  });
});
