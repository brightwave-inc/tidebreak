import { CornerDownLeft } from "lucide-react";

import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { STATUS_DOT } from "./code/statusTone";
import {
  PALETTE_PREFIXES,
  type PaletteGroup,
  type PaletteRow,
} from "./CommandPalette";
import {
  shellShortcutFor,
  shortcutKeycaps,
  usesCommandModifier,
  type ShellShortcutMode,
} from "./ShellShortcuts";

/**
 * The palette's list, with no idea where its rows came from.
 *
 * Split from the wired dialog for the reason `ShortcutsList` is split from
 * `ShortcutsDialog`: a story can draw every state of it — loading, empty, a
 * scoped query — without standing up a router, a client, or a catalog.
 * Everything here is props.
 */
export function CommandPaletteList({
  groups,
  query,
  onQueryChange,
  onSelect,
  scopeLabel = null,
  mode = "code",
  loading = false,
  emptyLabel = "Nothing here matches that.",
  command = usesCommandModifier(navigator.userAgent),
}: {
  groups: readonly PaletteGroup[];
  query: string;
  onQueryChange: (query: string) => void;
  onSelect: (row: PaletteRow) => void;
  /** The prefix's own word, drawn as a chip so the scope is never invisible. */
  scopeLabel?: string | null;
  /** Which mode's chords the rows draw. */
  mode?: ShellShortcutMode;
  loading?: boolean;
  emptyLabel?: string;
  /** Fixed per story so keycaps look the same wherever they are opened. */
  command?: boolean;
}) {
  return (
    <Command
      shouldFilter={false}
      label="Search commands, workspaces, and settings"
      className="rounded-none bg-transparent"
      // Backspace on an empty query drops the scope, which is how the reader
      // gets out of one without reaching for Escape and losing the whole
      // palette.
      onKeyDown={(event) => {
        if (event.key !== "Backspace" || query.length > 0) return;
        if (!scopeLabel) return;
        event.preventDefault();
        onQueryChange("");
      }}
    >
      {/* Block, not a flex row: the field and the rule under it have to span
          the palette's whole width, and a flex sibling beside them would make
          both stop short. The esc cap and the spinner float over the field's
          right end instead, with the text padded clear of them. */}
      <div className="relative">
        <CommandInput
          value={query}
          onValueChange={onQueryChange}
          placeholder="Type a command or search…"
          autoComplete="off"
          spellCheck={false}
          className="h-12 pr-20"
          leading={
            scopeLabel ? (
              <span className="shrink-0 rounded bg-muted px-1.5 py-1 text-xs text-muted-foreground">
                {scopeLabel}
              </span>
            ) : undefined
          }
        />
        {loading && (
          <Spinner
            className="absolute top-1/2 right-12 size-4 -translate-y-1/2"
            aria-label="Loading"
          />
        )}
        <kbd className="absolute top-1/2 right-3 -translate-y-1/2 shrink-0 rounded border px-1.5 py-0.5 font-sans text-2xs text-muted-foreground">
          esc
        </kbd>
      </div>

      <CommandList className="max-h-[min(60vh,32rem)] min-h-24 p-1.5">
        <CommandEmpty className="px-3 py-6 text-sm text-muted-foreground">
          {emptyLabel}
        </CommandEmpty>
        {groups.map((group) => (
          <CommandGroup
            key={group.section}
            heading={group.label}
            className="p-0"
          >
            {group.rows.map((row) => (
              <PaletteItem
                key={row.id}
                row={row}
                mode={mode}
                command={command}
                onSelect={onSelect}
              />
            ))}
          </CommandGroup>
        ))}
      </CommandList>

      <PaletteFooter scoped={Boolean(scopeLabel)} />
    </Command>
  );
}

function PaletteItem({
  row,
  mode,
  command,
  onSelect,
}: {
  row: PaletteRow;
  mode: ShellShortcutMode;
  command: boolean;
  onSelect: (row: PaletteRow) => void;
}) {
  const Icon = row.icon;
  const def = row.shortcut ? shellShortcutFor(row.shortcut, mode) : null;
  const caps = def ? shortcutKeycaps(def, command) : [];
  return (
    <CommandItem
      value={row.id}
      aria-label={row.label}
      onSelect={() => onSelect(row)}
      className="gap-2.5 px-2.5 py-2"
    >
      {row.tone ? (
        <span
          className={cn(
            "h-4 w-0.5 shrink-0 rounded-full",
            STATUS_DOT[row.tone],
          )}
          aria-hidden
        />
      ) : (
        Icon && (
          <Icon className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        )
      )}
      <span className="min-w-0 flex-1 truncate text-sm">{row.label}</span>
      {row.hint && (
        <span className="min-w-0 max-w-[45%] shrink-0 truncate text-xs text-muted-foreground">
          {row.hint}
        </span>
      )}
      {caps.length > 0 && (
        <span className="flex shrink-0 items-center gap-0.5">
          {caps.map((cap) => (
            <kbd
              key={cap}
              className="inline-flex h-5 min-w-5 items-center justify-center rounded border bg-muted/60 px-1 font-sans text-2xs leading-none text-muted-foreground"
            >
              {cap}
            </kbd>
          ))}
        </span>
      )}
    </CommandItem>
  );
}

/**
 * The prefixes, spelled out.
 *
 * They are the palette's only invisible affordance, so they live here rather
 * than in a tour nobody reads. Inside a scope the row explains the way out
 * instead, which is the thing a reader in one actually needs.
 */
function PaletteFooter({ scoped }: { scoped: boolean }) {
  return (
    <div className="flex items-center justify-between gap-3 border-t px-3 py-1.5 text-2xs text-muted-foreground">
      <span className="flex items-center gap-2.5">
        {scoped ? (
          <span>
            <kbd className="font-sans">⌫</kbd> back
          </span>
        ) : (
          PALETTE_PREFIXES.map((prefix) => (
            <span key={prefix.char}>
              <kbd className="font-sans">{prefix.char}</kbd> {prefix.label}
            </span>
          ))
        )}
      </span>
      <span className="flex items-center gap-2.5">
        <span>↑↓ navigate</span>
        <span className="flex items-center gap-1">
          <CornerDownLeft className="size-3" aria-hidden /> run
        </span>
      </span>
    </div>
  );
}
