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
import { shellShortcutMode } from "./code/routes";
import {
  groupedShellShortcuts,
  shortcutKeycaps,
  usesCommandModifier,
  type ShellShortcutDef,
  type ShellShortcutMode,
} from "./ShellShortcuts";

function Keycap({ children }: { children: string }) {
  return (
    <kbd className="inline-flex h-6 min-w-6 items-center justify-center rounded border bg-muted/60 px-1.5 font-sans text-2xs leading-none font-medium text-foreground/80">
      {children}
    </kbd>
  );
}

function ShortcutRow({
  shortcut,
  command,
}: {
  shortcut: ShellShortcutDef;
  command: boolean;
}) {
  const caps = shortcutKeycaps(shortcut, command);
  return (
    <>
      <span className="truncate text-sm text-foreground/90">
        {shortcut.description}
      </span>
      <span className="flex shrink-0 items-center gap-1">
        {caps.map((cap) => (
          <Keycap key={cap}>{cap}</Keycap>
        ))}
      </span>
    </>
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
    <div className="grid max-h-[65vh] grid-cols-[1fr_auto] items-center gap-x-6 gap-y-2 overflow-y-auto pr-1">
      {groups.map(({ group, items }, index) => (
        <Fragment key={group}>
          <h3
            className={cn(
              "col-span-2 text-2xs font-semibold tracking-[0.08em] text-muted-foreground uppercase",
              index > 0 && "mt-4",
            )}
          >
            {group}
          </h3>
          {items.map((shortcut) => (
            <ShortcutRow
              key={`${shortcut.id}:${shortcutKeycaps(shortcut, command).join("")}`}
              shortcut={shortcut}
              command={command}
            />
          ))}
        </Fragment>
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
            These act on the app frame, so they work from every screen.
          </DialogDescription>
        </DialogHeader>
        <ShortcutsList mode={mode} />
      </DialogContent>
    </Dialog>
  );
}
