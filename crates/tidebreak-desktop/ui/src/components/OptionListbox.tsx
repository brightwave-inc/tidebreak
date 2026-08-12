import type { LucideIcon } from "lucide-react";

import { cn } from "@/lib/utils";

/**
 * One row of a composer popover, as the list needs it.
 *
 * Deliberately not a plugin, a file, or anything else the composer can reach:
 * the popover is the same widget whichever trigger opened it — `/` for the
 * plugin library today — so it takes rows and reports the pick, and each
 * consumer keeps its own vocabulary on its own side of that line.
 */
export type OptionRow = {
  /** Stable across renders and unique within the list. */
  key: string;
  label: string;
  description?: string;
  icon: LucideIcon;
  /** A word on the right saying what picking this row will do. */
  hint?: string;
  /**
   * The row is shown for context but cannot be picked. Its `hint` carries the
   * reason, which is the only thing making a dimmed row worth showing at all.
   */
  disabled?: boolean;
};

/**
 * A listbox driven from somewhere else's keyboard.
 *
 * The field with focus — the draft textarea for `/`, the panel's own search box
 * when a menu opened it — keeps the keys and points at the active row through
 * `aria-activedescendant`, which is why the highlight is a prop rather than
 * this component's own state.
 */
export function OptionListbox({
  listId,
  label,
  rows,
  activeIndex,
  note,
  onPick,
  onHighlight,
}: {
  listId: string;
  label: string;
  rows: readonly OptionRow[];
  activeIndex: number;
  /** A line under the rows explaining a list that is short for a reason. */
  note?: string | null;
  onPick: (index: number) => void;
  onHighlight: (index: number) => void;
}) {
  return (
    <ul
      id={listId}
      role="listbox"
      aria-label={label}
      className="m-0 max-h-56 list-none overflow-y-auto p-1"
    >
      {rows.map((row, index) => {
        const Icon = row.icon;
        return (
          <li key={row.key}>
            <button
              type="button"
              id={optionElementId(listId, index)}
              role="option"
              aria-selected={index === activeIndex}
              aria-disabled={row.disabled || undefined}
              className={cn(
                "flex w-full items-start gap-2 rounded-sm px-2 py-1.5 text-left text-sm",
                index === activeIndex && "bg-accent text-accent-foreground",
                row.disabled && "opacity-50",
              )}
              // Taken on mousedown so the field never loses the caret the
              // insertion is about to be made at.
              onMouseDown={(event) => event.preventDefault()}
              onMouseEnter={() => onHighlight(index)}
              onClick={() => {
                if (row.disabled) return;
                onPick(index);
              }}
            >
              <Icon
                className="mt-0.5 size-4 shrink-0 text-muted-foreground"
                aria-hidden="true"
              />
              <span className="flex min-w-0 flex-col">
                <span className="truncate">{row.label}</span>
                {row.description && (
                  <span className="truncate text-xs text-muted-foreground">
                    {row.description}
                  </span>
                )}
              </span>
              {row.hint && (
                // Never wraps: the row is one line, and the label beside it is
                // the part that gives way. A hint carrying a hyphen would
                // otherwise break across two lines the moment the list is
                // narrow, while the label sat there un-truncated.
                <span className="ml-auto shrink-0 pl-2 text-[0.68rem] whitespace-nowrap text-muted-foreground">
                  {row.hint}
                </span>
              )}
            </button>
          </li>
        );
      })}
      {note && (
        <li className="px-2 py-1.5 text-xs text-muted-foreground">{note}</li>
      )}
    </ul>
  );
}

/** The id a field's `aria-activedescendant` points at. */
export function optionElementId(listId: string, index: number): string {
  return `${listId}-option-${index}`;
}
