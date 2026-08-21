import { useEffect, useRef } from "react";
import { Sparkles, TerminalSquare, Wand2 } from "lucide-react";
import type { LucideIcon } from "lucide-react";

import {
  filterSlashOptions,
  MAX_INVOKED_SKILLS,
  type SlashOption,
} from "@/ComposerSlash";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { cn } from "@/lib/utils";
import { categoryIcon } from "./pluginVocabulary";
import type { OptionRow } from "@/components/OptionListbox";

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
export function pluginOptionRows(options: readonly SlashOption[]): OptionRow[] {
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
  const matches = filterSlashOptions(options, query);

  // Opened from a menu the pointer has just left: focus goes to the field so
  // the list can be driven by the keyboard from the moment it appears.
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  return (
    <Command
      shouldFilter={false}
      label={PLUGINS_PANEL_LABEL}
      className="rounded-md border border-border bg-popover text-popover-foreground shadow-md"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          onClose();
        }
      }}
      onBlur={(event) => {
        // A click on a row is taken on mousedown, which never moves focus, so
        // a blur that leaves the panel is the reader leaving for good.
        const next = event.relatedTarget;
        if (next instanceof Node && event.currentTarget.contains(next)) return;
        onClose();
      }}
    >
      <CommandInput
        ref={inputRef}
        value={query}
        onValueChange={onQueryChange}
        placeholder="Search plugins, skills, and prompts…"
        className="h-9"
      />
      <CommandList className="max-h-56">
        {matches.length === 0 && !capNote ? (
          <CommandEmpty className="px-3 py-2 text-xs text-muted-foreground">
            Nothing matches.
          </CommandEmpty>
        ) : (
          <CommandGroup className="p-1">
            {matches.map((option) => {
              const Icon = optionIcon(option);
              const disabled = option.unavailable !== undefined;
              const hint = option.unavailable ?? optionKindLabel(option);
              return (
                <CommandItem
                  key={`${option.kind}:${option.name}`}
                  value={`${option.kind}:${option.name}`}
                  disabled={disabled}
                  onMouseDown={(event) => event.preventDefault()}
                  onSelect={() => {
                    if (disabled) return;
                    onPick(option);
                  }}
                  className={cn(disabled && "opacity-50")}
                >
                  <Icon
                    className="size-4 shrink-0 text-muted-foreground"
                    aria-hidden="true"
                  />
                  <span className="flex min-w-0 flex-col">
                    <span className="truncate">{option.label}</span>
                    {option.description && (
                      <span className="truncate text-xs text-muted-foreground">
                        {option.description}
                      </span>
                    )}
                  </span>
                  {hint && (
                    <span className="ml-auto shrink-0 pl-2 text-[0.68rem] whitespace-nowrap text-muted-foreground">
                      {hint}
                    </span>
                  )}
                </CommandItem>
              );
            })}
            {capNote && (
              <div className="px-2 py-1.5 text-xs text-muted-foreground">
                {skillCapNote()}
              </div>
            )}
          </CommandGroup>
        )}
      </CommandList>
    </Command>
  );
}

export const PLUGINS_PANEL_LABEL = "Plugins, skills, and prompts";

/**
 * A bundle has no icon of its own, so the category stands in for one — for the
 * bundle and for the skills inside it, which is what makes a chip recognisable
 * as having come from a particular library.
 */
export function optionIcon(option: SlashOption): LucideIcon {
  if (option.kind === "command") return TerminalSquare;
  if (option.kind === "prompt") return Sparkles;
  if (option.category) return categoryIcon(option.category);
  return Wand2;
}

function optionKindLabel(option: SlashOption): string {
  return option.kind === "plugin"
    ? "Plugin"
    : option.kind === "skill"
      ? "Skill"
      : option.kind === "command"
        ? "Command"
        : "Prompt";
}
