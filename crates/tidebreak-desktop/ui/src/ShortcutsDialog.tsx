import { Fragment, useMemo } from "react";
import { useRouterState } from "@tanstack/react-router";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import { DIFF_KEYS } from "./code/diff/diffKeys";
import { shellShortcutMode } from "./code/routes";
import { COMPOSER_KEYS } from "./ComposerKeys";
import {
  groupedShellShortcuts,
  shortcutKeycaps,
  usesCommandModifier,
  type ShellShortcutMode,
} from "./ShellShortcuts";

function Keycap({ children }: { children: string }) {
  return (
    <kbd className="inline-flex h-6 min-w-6 items-center justify-center rounded border bg-muted/60 px-1.5 font-sans text-2xs leading-none font-medium text-foreground">
      {children}
    </kbd>
  );
}

function ShortcutRow({
  description,
  caps,
  alternate,
}: {
  description: string;
  caps: readonly string[];
  /** Another key that does the same. */
  alternate?: readonly string[];
}) {
  return (
    <>
      <span className="truncate text-sm text-foreground/90">{description}</span>
      <span className="flex shrink-0 items-center gap-1">
        {caps.map((cap) => (
          <Keycap key={cap}>{cap}</Keycap>
        ))}
        {alternate && alternate.length > 0 && (
          <>
            <span className="px-0.5 text-xs text-muted-foreground">or</span>
            {alternate.map((cap) => (
              <Keycap key={`or-${cap}`}>{cap}</Keycap>
            ))}
          </>
        )}
      </span>
    </>
  );
}

function GroupHeading({
  first,
  children,
}: {
  first: boolean;
  children: string;
}) {
  return (
    <h3
      className={cn(
        "col-span-2 text-2xs font-semibold tracking-[0.08em] text-muted-foreground uppercase",
        !first && "mt-4",
      )}
    >
      {children}
    </h3>
  );
}

/**
 * What the keyboard can reach, read straight out of `SHELL_SHORTCUTS`.
 *
 * Rendering from the table the listener matches on is the point: a hand-written
 * second copy would drift the first time a binding changed, and a help dialog
 * that misstates the keys is worse than no dialog at all. Listed for the mode
 * asked for, for the same reason: Cmd+N is one row, and which one is true
 * depends on where the reader pressed it.
 *
 * The composer's own keys close the list. The composer answers them itself
 * rather than through the shell table, and both halves of the app share them,
 * so they come from `COMPOSER_KEYS` and are listed in every mode. Code mode
 * also lists the diff's keys, from `DIFF_KEYS`, which the diff answers while
 * it has focus.
 *
 * Split from the dialog so a story can draw both modes without standing up a
 * router to answer which one the reader is in.
 */
export function ShortcutsList({
  mode,
  command = usesCommandModifier(navigator.userAgent),
}: {
  mode: ShellShortcutMode;
  command?: boolean;
}) {
  const groups = useMemo(() => groupedShellShortcuts(mode), [mode]);
  return (
    <div
      className="grid max-h-[65vh] grid-cols-[1fr_auto] items-center gap-x-6 gap-y-2 overflow-y-auto pr-1"
      tabIndex={0}
      aria-label="Keyboard shortcuts"
    >
      {groups.map(({ group, items }, index) => (
        <Fragment key={group}>
          <GroupHeading first={index === 0}>{group}</GroupHeading>
          {items.map((shortcut) => {
            const caps = shortcutKeycaps(shortcut, command);
            return (
              <ShortcutRow
                key={`${shortcut.id}:${caps.join("")}`}
                description={shortcut.description}
                caps={caps}
              />
            );
          })}
        </Fragment>
      ))}
      {mode === "code" && (
        <>
          <GroupHeading first={groups.length === 0}>Diff</GroupHeading>
          {DIFF_KEYS.map((key) => (
            <ShortcutRow
              key={key.id}
              description={key.description}
              caps={key.keycaps(command)}
              alternate={key.alternate}
            />
          ))}
        </>
      )}
      <GroupHeading first={groups.length === 0 && mode !== "code"}>
        Composer
      </GroupHeading>
      {COMPOSER_KEYS.map((key) => (
        <ShortcutRow
          key={key.id}
          description={key.description}
          caps={key.keycaps(command)}
        />
      ))}
    </div>
  );
}

/** The help dialog, listing whichever mode the route puts the reader in. */
export function ShortcutsDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const mode = useRouterState({
    select: (state) => shellShortcutMode(state.location.pathname),
  });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md gap-6">
        <DialogHeader>
          <DialogTitle>Keyboard shortcuts</DialogTitle>
          <DialogDescription>
            Most of these work from every screen. The composer keys work while
            you write a message.
          </DialogDescription>
        </DialogHeader>
        <ShortcutsList mode={mode} />
      </DialogContent>
    </Dialog>
  );
}
