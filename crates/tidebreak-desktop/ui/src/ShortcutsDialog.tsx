import { Fragment, useMemo } from "react";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import {
  groupedShellShortcuts,
  shortcutKeycaps,
  usesCommandModifier,
  type ShellShortcutDef,
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
 * that misstates the keys is worse than no dialog at all.
 */
export function ShortcutsDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const command = useMemo(() => usesCommandModifier(navigator.userAgent), []);
  const groups = useMemo(() => groupedShellShortcuts(), []);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md gap-6">
        <DialogHeader>
          <DialogTitle>Keyboard shortcuts</DialogTitle>
          <DialogDescription>
            These act on the app frame, so they work from every screen.
          </DialogDescription>
        </DialogHeader>
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
                  key={shortcut.id}
                  shortcut={shortcut}
                  command={command}
                />
              ))}
            </Fragment>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
