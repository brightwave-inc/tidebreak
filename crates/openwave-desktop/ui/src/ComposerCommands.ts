import type { SlashOption } from "./ComposerSlash";

/**
 * A command the app runs itself.
 *
 * Commands are not skills: nothing is staged onto the next message and no turn
 * is sent. Picking one does something here in the renderer, which is why they
 * carry their own kind through the `/` list rather than being modelled as a
 * library entry that happens to be built in.
 */
export type SlashCommand = {
  /** The word typed after `/`. Lowercase; no spaces — the token ends at one. */
  name: SlashCommandName;
  description: string;
  /**
   * Whether the rest of the line is meaningful to this command. A command that
   * takes none is only a command when nothing follows it, so an ordinary
   * sentence that happens to open with its name is still sent as a message.
   */
  takesArgument: boolean;
};

export type SlashCommandName = "usage";

/**
 * Every built-in command, in the order the list offers them.
 *
 * These names are reserved: a skill or prompt sharing one is not offered under
 * it, so what `/name` does never depends on which libraries are installed.
 */
export const SLASH_COMMANDS: readonly SlashCommand[] = [
  {
    name: "usage",
    description: "Show this chat's context and token usage.",
    takesArgument: false,
  },
];

/** The command rows for the `/` list, ahead of anything a library offers. */
export function commandSlashOptions(
  commands: readonly SlashCommand[] = SLASH_COMMANDS,
): SlashOption[] {
  return commands.map((command) => ({
    kind: "command",
    name: command.name,
    label: `/${command.name}`,
    description: command.description,
  }));
}

/**
 * The library's rows with the built-in commands in front of them.
 *
 * Commands lead because their names are reserved, and a catalog entry sharing
 * one is dropped rather than shown below it: two rows reading `usage` would
 * leave the reader guessing which one Enter takes, and the answer would change
 * as plugins came and went.
 */
export function withSlashCommands(
  options: readonly SlashOption[],
  commands: readonly SlashCommand[] = SLASH_COMMANDS,
): SlashOption[] {
  const reserved = new Set(commands.map((command) => command.name as string));
  return [
    ...commandSlashOptions(commands),
    ...options.filter((option) => !reserved.has(option.name)),
  ];
}

/** A command named by the draft, with whatever the reader typed after it. */
export type ParsedSlashCommand = {
  command: SlashCommand;
  /** The rest of the line, trimmed. Empty when the command stands alone. */
  argument: string;
};

/**
 * The command a draft invokes, if it invokes one.
 *
 * Only at the very start of the draft: `/usage` is a command, "see /usage" is a
 * sentence. The name has to end the draft or be followed by whitespace, so
 * `/usagex` is not `/usage`, and a command that takes no argument gives the
 * draft back to the composer once anything follows it — the reader who typed a
 * paragraph beginning with a command name meant to send it.
 */
export function parseSlashCommand(
  draft: string,
  commands: readonly SlashCommand[] = SLASH_COMMANDS,
): ParsedSlashCommand | null {
  const text = draft.trimStart();
  if (!text.startsWith("/")) return null;
  const separator = text.search(/\s/);
  const name = (separator < 0 ? text : text.slice(0, separator)).slice(1);
  const command = commands.find((candidate) => candidate.name === name);
  if (!command) return null;
  const argument = separator < 0 ? "" : text.slice(separator).trim();
  if (argument && !command.takesArgument) return null;
  return { command, argument };
}
