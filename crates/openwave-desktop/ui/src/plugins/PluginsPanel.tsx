import { type KeyboardEvent, useEffect, useRef, useState } from "react";
import { Search, Sparkles, Wand2 } from "lucide-react";
import type { LucideIcon } from "lucide-react";

import {
  filterSlashOptions,
  nextOptionHighlight,
  MAX_INVOKED_SKILLS,
  type SlashOption,
} from "@/ComposerSlash";
import { categoryIcon } from "./pluginVocabulary";
import { OptionListbox, optionElementId, type OptionRow } from "@/components/OptionListbox";

/**
 * The library, as one list.
 *
 * Both ways into it — typing `/` in the draft and picking Plugins from the
 * tools menu — render these same rows and pick with the same code, so a bundle
 * reached one way behaves exactly as it does the other. A flat list rather than
 * a section per kind: the reader is looking for a name, and the kind only
 * decides what picking does, which the row says on its right. A row that cannot
 * be picked right now says why there instead.
 */
export function pluginOptionRows(
  options: readonly SlashOption[],
): OptionRow[] {
  return options.map((option) => ({
    key: `${option.kind}:${option.name}`,
    label: option.label,
    description: option.description,
    icon: optionIcon(option),
    hint: option.unavailable ?? optionKindLabel(option),
    disabled: option.unavailable !== undefined,
  }));
}

/** The line under the rows when the turn's cap has taken the skills away. */
export function skillCapNote(): string {
  return `A message can invoke at most ${MAX_INVOKED_SKILLS} skills.`;
}

/**
 * The same list opened from the tools menu, with a field to narrow it.
 *
 * The `/` list is already being typed into, so it needs no field of its own;
 * this one is opened by a pointer and would otherwise have nowhere to type. It
 * sits in the composer's flow above the draft rather than floating: the
 * composer clips its own overflow to keep its rounded edge, so a panel anchored
 * over the box would be cut off at the border.
 */
export function PluginsPanel({
  options,
  query,
  capNote = false,
  onQueryChange,
  onPick,
  onClose,
}: {
  options: readonly SlashOption[];
  query: string;
  capNote?: boolean;
  onQueryChange: (query: string) => void;
  onPick: (option: SlashOption) => void;
  onClose: () => void;
}) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const [highlight, setHighlight] = useState(0);
  const matches = filterSlashOptions(options, query);
  const index = Math.min(highlight, matches.length - 1);

  // Opened from a menu the pointer has just left: focus goes to the field so
  // the list can be driven by the keyboard from the moment it appears.
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    const moved = nextOptionHighlight(event.key, index, matches.length);
    if (moved !== null) {
      event.preventDefault();
      setHighlight(moved);
      return;
    }
    if (event.key === "Enter" || event.key === "Tab") {
      const option = matches[index];
      if (!option) return;
      event.preventDefault();
      // A row that cannot be picked leaves the panel exactly as it is; its hint
      // already says what is in the way.
      if (option.unavailable) return;
      onPick(option);
    }
  }

  return (
    <div className="rounded-md border border-border bg-popover text-popover-foreground shadow-md">
      <div className="flex items-center gap-2 border-b border-border px-2 py-1.5">
        <Search
          className="size-4 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
        <input
          ref={inputRef}
          type="text"
          className="w-full border-none bg-transparent text-sm placeholder:text-muted-foreground focus-visible:outline-none"
          placeholder="Search plugins, skills, and prompts…"
          aria-label="Search the plugin library"
          aria-controls={PLUGINS_PANEL_LIST_ID}
          aria-activedescendant={
            matches.length > 0
              ? optionElementId(PLUGINS_PANEL_LIST_ID, index)
              : undefined
          }
          value={query}
          onChange={(event) => {
            onQueryChange(event.target.value);
            setHighlight(0);
          }}
          // A click on a row is taken on mousedown, which never moves focus, so
          // a blur here is the reader leaving the panel for good.
          onBlur={onClose}
          onKeyDown={onKeyDown}
        />
      </div>
      {matches.length === 0 && !capNote ? (
        <p className="px-3 py-2 text-xs text-muted-foreground">
          Nothing matches.
        </p>
      ) : (
        <OptionListbox
          listId={PLUGINS_PANEL_LIST_ID}
          label={PLUGINS_PANEL_LABEL}
          rows={pluginOptionRows(matches)}
          activeIndex={index}
          note={capNote ? skillCapNote() : null}
          onPick={(picked) => {
            const option = matches[picked];
            if (option) onPick(option);
          }}
          onHighlight={setHighlight}
        />
      )}
    </div>
  );
}

export const PLUGINS_PANEL_LABEL = "Plugins, skills, and prompts";

const PLUGINS_PANEL_LIST_ID = "composer-plugins-list";

/**
 * A bundle has no icon of its own, so the category stands in for one — for the
 * bundle and for the skills inside it, which is what makes a chip recognisable
 * as having come from a particular library.
 */
export function optionIcon(option: SlashOption): LucideIcon {
  if (option.kind === "prompt") return Sparkles;
  if (option.category) return categoryIcon(option.category);
  return Wand2;
}

function optionKindLabel(option: SlashOption): string {
  return option.kind === "plugin"
    ? "Plugin"
    : option.kind === "skill"
      ? "Skill"
      : "Prompt";
}
