import type { PluginCatalog, PluginCategory } from "./api";
import { promptTitle } from "./WelcomeState";

/** The server's bound on how many skills one turn may invoke. */
export const MAX_INVOKED_SKILLS = 8;

/**
 * One thing the plugins panel can reach: a skill to invoke, a bundle of them,
 * or a prompt to insert.
 *
 * All three live in one flat list because the reader is looking for a name, not
 * deciding which library it came from. `kind` is what the composer acts on:
 * picking a skill leaves a pill behind, picking a bundle leaves one per member
 * skill, and picking a prompt writes text.
 */
export type SlashOption = {
  kind: "skill" | "prompt" | "plugin";
  /** The catalog slug — what the wire and the pill are keyed by. */
  name: string;
  label: string;
  description: string;
  /** The owning bundle's category, where one is known, standing in for an icon. */
  category?: PluginCategory;
  /** A bundle row's invocable member skills, in manifest order. */
  skills?: readonly string[];
  /**
   * Whether a bundle row's own flag is on. A row is offered either way at rest,
   * because picking a disabled one turns it on; mid-turn that is not available,
   * so the flag is what tells the two cases apart.
   */
  enabled?: boolean;
};

/**
 * The trigger token being typed, if the caret is inside one.
 *
 * A trigger only opens a list at the start of the draft or after whitespace, so
 * ordinary text — a path, a date, an and/or, an email address — never raises a
 * popover over what someone is writing. Whitespace ends the token: once a space
 * is typed the reader has moved on to prose, and nothing any of these lists
 * offers is addressed by a name with a space in it.
 *
 * The character is a parameter because the draft has more than one thing to
 * reach for; `/` is the plugin library.
 */
export function activeTokenQuery(
  draft: string,
  caret: number,
  trigger: string,
): { start: number; query: string } | null {
  const upToCaret = draft.slice(0, caret);
  const start = upToCaret.lastIndexOf(trigger);
  if (start < 0) return null;
  const preceding = start === 0 ? "" : draft[start - 1];
  if (preceding && !/\s/.test(preceding)) return null;
  const query = upToCaret.slice(start + trigger.length);
  if (/\s/.test(query)) return null;
  return { start, query };
}

/** The `/` token under the caret: the way into the plugin library. */
export function activeSlashQuery(
  draft: string,
  caret: number,
): { start: number; query: string } | null {
  return activeTokenQuery(draft, caret, "/");
}


/**
 * The options a query names, best match first.
 *
 * Substring rather than fuzzy: the reader is completing a name they mostly
 * know, and a fuzzy match over two libraries returns rows nobody typed toward.
 * A name that starts with the query sorts above one that merely contains it,
 * and a description-only match sorts last — it is the weakest evidence that
 * this is the row being reached for.
 */
export function filterSlashOptions(
  options: readonly SlashOption[],
  query: string,
): SlashOption[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [...options];
  const ranked: { option: SlashOption; rank: number }[] = [];
  for (const option of options) {
    const name = option.name.toLowerCase();
    const label = option.label.toLowerCase();
    const rank = name.startsWith(needle) || label.startsWith(needle)
      ? 0
      : name.includes(needle) || label.includes(needle)
        ? 1
        : option.description.toLowerCase().includes(needle)
          ? 2
          : -1;
    if (rank >= 0) ranked.push({ option, rank });
  }
  return ranked
    .sort((left, right) => left.rank - right.rank)
    .map((entry) => entry.option);
}

/**
 * The draft with the `/query` token taken out and `replacement` put in its
 * place, and where the caret lands after it.
 */
export function replaceSlashToken(
  draft: string,
  start: number,
  caret: number,
  replacement: string,
): { text: string; caret: number } {
  const before = draft.slice(0, start);
  const after = draft.slice(caret);
  return {
    text: `${before}${replacement}${after}`,
    caret: before.length + replacement.length,
  };
}

/**
 * What the catalog offers the panel: every enabled skill, bundled or standing
 * alone; every installed bundle that carries one; and every enabled prompt.
 *
 * A member skill is offered on its own only when its bundle is on as well as
 * itself — a skill inside a disabled bundle is not staged for a turn, so
 * offering it would produce a send the server refuses. The bundle row is
 * offered either way: picking one turns it on and then invokes its members, so
 * the row is how a reader reaches a library they have installed but not
 * enabled. A bundle carrying no enabled skill has no row — turning it on would
 * leave nothing on the message to see.
 */
export function slashOptionsFromCatalog(
  catalog: PluginCatalog | null,
): SlashOption[] {
  if (!catalog) return [];
  const plugins: SlashOption[] = [];
  const skills: SlashOption[] = [];
  const seen = new Set<string>();
  const addSkill = (
    skill: { name: string; description: string },
    category?: PluginCategory,
  ) => {
    if (seen.has(skill.name)) return;
    seen.add(skill.name);
    skills.push({
      kind: "skill",
      name: skill.name,
      label: promptTitle(skill.name),
      description: skill.description,
      category,
    });
  };
  for (const plugin of catalog.plugins) {
    const members = plugin.skills
      .filter((skill) => skill.enabled)
      .map((skill) => skill.name);
    if (members.length > 0) {
      plugins.push({
        kind: "plugin",
        name: plugin.name,
        label: plugin.display_name,
        description: plugin.description,
        category: plugin.category,
        skills: members,
        enabled: plugin.enabled,
      });
    }
    if (!plugin.enabled) continue;
    for (const skill of plugin.skills) {
      if (skill.enabled) addSkill(skill, plugin.category);
    }
  }
  for (const skill of catalog.skills) {
    if (skill.enabled) addSkill(skill);
  }
  const prompts: SlashOption[] = catalog.prompts
    .filter((prompt) => prompt.enabled)
    .map((prompt) => ({
      kind: "prompt",
      name: prompt.name,
      label: promptTitle(prompt.name),
      description: prompt.description,
    }));
  return [...plugins, ...skills, ...prompts];
}

/**
 * The skills a pick would put on the message.
 *
 * A bundle stands for its members, so picking one is picking each of them: the
 * chips it leaves are the chips those skills would have left individually. What
 * is already on the message is skipped rather than repeated — the server
 * refuses a duplicate — and the turn's cap truncates rather than rejects, so a
 * large bundle fills the remaining room instead of doing nothing.
 */
export function skillsToInvoke(
  option: SlashOption,
  invoked: readonly string[],
): string[] {
  const names =
    option.kind === "plugin"
      ? (option.skills ?? [])
      : option.kind === "skill"
        ? [option.name]
        : [];
  const picked: string[] = [];
  for (const name of names) {
    if (invoked.length + picked.length >= MAX_INVOKED_SKILLS) break;
    if (invoked.includes(name) || picked.includes(name)) continue;
    picked.push(name);
  }
  return picked;
}

/**
 * What the panel can still reach, given what this message already carries.
 *
 * A skill already on the message is not offered again, and neither is anything
 * that invokes once the cap is reached. A steer carries its own invocation
 * under its own budget, so skills stay reachable while a turn runs — but a
 * *disabled* bundle's row does not: picking one turns the bundle on
 * install-wide and names a manifest the running turn's workspace never staged.
 * Prompts are text and stay available throughout.
 */
export function availableSlashOptions(
  options: readonly SlashOption[],
  invoked: readonly string[],
  { steering }: { steering: boolean },
): SlashOption[] {
  return options.filter((option) => {
    if (option.kind === "prompt") return true;
    if (steering && option.kind === "plugin" && option.enabled === false) {
      return false;
    }
    if (invoked.length >= MAX_INVOKED_SKILLS) return false;
    return skillsToInvoke(option, invoked).length > 0;
  });
}

/**
 * Where an arrow key moves the highlight, or `null` when the key is not the
 * list's to take. Shared so every popover the composer raises moves alike.
 */
export function nextOptionHighlight(
  key: string,
  current: number,
  count: number,
): number | null {
  if (count === 0) return null;
  const bounded = Math.min(Math.max(current, 0), count - 1);
  if (key === "ArrowDown") return (bounded + 1) % count;
  if (key === "ArrowUp") return (bounded + count - 1) % count;
  return null;
}
