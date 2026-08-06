import type { PluginCatalog } from "./api";
import { promptTitle } from "./WelcomeState";

/** The server's bound on how many skills one turn may invoke. */
export const MAX_INVOKED_SKILLS = 8;

/**
 * One thing a `/` can reach: a skill to invoke, or a prompt to insert.
 *
 * Both live in one flat list because the reader is looking for a name, not
 * deciding which library it came from. `kind` is what the composer acts on:
 * picking a skill leaves a pill behind, picking a prompt writes text.
 */
export type SlashOption = {
  kind: "skill" | "prompt";
  /** The catalog slug — what the wire and the pill are keyed by. */
  name: string;
  label: string;
  description: string;
};

/**
 * The `/` token being typed, if the caret is inside one.
 *
 * A `/` only opens the list at the start of the draft or after whitespace, so
 * ordinary text — a path, a date, an and/or — never raises a popover over what
 * someone is writing. Whitespace ends the token: once a space is typed the
 * reader has moved on to prose, and nothing in either catalog is addressed by
 * a name with a space in it.
 */
export function activeSlashQuery(
  draft: string,
  caret: number,
): { start: number; query: string } | null {
  const upToCaret = draft.slice(0, caret);
  const start = upToCaret.lastIndexOf("/");
  if (start < 0) return null;
  const preceding = start === 0 ? "" : draft[start - 1];
  if (preceding && !/\s/.test(preceding)) return null;
  const query = upToCaret.slice(start + 1);
  if (/\s/.test(query)) return null;
  return { start, query };
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
 * What the catalog offers a `/` today: every enabled skill, bundled or
 * standing alone, and every enabled prompt.
 *
 * A member skill is offered only when its bundle is on as well as itself —
 * a skill inside a disabled bundle is not staged for a turn, so offering it
 * would produce a send the server refuses.
 */
export function slashOptionsFromCatalog(
  catalog: PluginCatalog | null,
): SlashOption[] {
  if (!catalog) return [];
  const skills: SlashOption[] = [];
  const seen = new Set<string>();
  const addSkill = (skill: { name: string; description: string }) => {
    if (seen.has(skill.name)) return;
    seen.add(skill.name);
    skills.push({
      kind: "skill",
      name: skill.name,
      label: promptTitle(skill.name),
      description: skill.description,
    });
  };
  for (const plugin of catalog.plugins) {
    if (!plugin.enabled) continue;
    for (const skill of plugin.skills) {
      if (skill.enabled) addSkill(skill);
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
  return [...skills, ...prompts];
}
