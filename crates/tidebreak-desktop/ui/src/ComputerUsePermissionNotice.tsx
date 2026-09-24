import { ShieldAlert, X } from "lucide-react";
import { useId } from "react";

import { Button } from "@/components/ui/button";
import type { PermissionRequired } from "./computerUsePermissionAsk";
import { PERMISSION_LABELS } from "./settings/ComputerUsePermissionRows";

/**
 * The notice's words for what a task was missing. `taskName` is the task's
 * title where the window knows it.
 */
export function permissionNoticeCopy(
  need: PermissionRequired,
  taskName: string | null = null,
): {
  title: string;
  body: string;
} {
  const task = taskName ? `“${taskName}”` : "A task";
  const feature = need.browser ? "Browser control" : "Computer use";
  const control = need.browser
    ? "click and type in your browser"
    : "click and type in other apps";
  const capture = need.browser
    ? "take screenshots of your browser window"
    : "take screenshots of other apps";
  switch (need.permission) {
    case "accessibility":
      return {
        title: `${feature} needs ${PERMISSION_LABELS.accessibility}`,
        body: `${task} stopped because macOS has not allowed Tidebreak to ${control}. Allow ${PERMISSION_LABELS.accessibility}, then ask the task to try again.`,
      };
    case "screen_recording":
      return {
        title: `${feature} needs ${PERMISSION_LABELS.screen_recording}`,
        body: `${task} stopped because macOS has not allowed Tidebreak to ${capture}. Allow ${PERMISSION_LABELS.screen_recording}, then ask the task to try again.`,
      };
    case null:
      return {
        title: `${feature} needs a macOS permission`,
        body: `${task} stopped because macOS has not allowed Tidebreak a permission it needs. Check ${PERMISSION_LABELS.accessibility} and ${PERMISSION_LABELS.screen_recording}, then ask the task to try again.`,
      };
  }
}

/**
 * What a task that stopped for a missing macOS permission says, so it never
 * fails silently: which task, which permission, what it enables, and the two
 * ways to allow it. Each task leaves its own. It stays after Not now, because
 * it is the way back that does not interrupt.
 */
export function ComputerUsePermissionNotice({
  need,
  taskName = null,
  onAllow,
  onOpenSettings,
  onDismiss,
}: {
  need: PermissionRequired;
  /** The task's title, when the window knows it. */
  taskName?: string | null;
  onAllow: () => void;
  /** Go to Tidebreak's Settings → Permissions. */
  onOpenSettings: () => void;
  onDismiss: () => void;
}) {
  const copy = permissionNoticeCopy(need, taskName);
  const bodyId = useId();
  // Shaped like the other notices it stacks with; the icon carries the tone.
  return (
    <aside
      className="relative rounded-xl border border-border bg-popover p-4 text-popover-foreground shadow-lg"
      aria-label={copy.title}
      aria-describedby={bodyId}
    >
      <button
        type="button"
        className="absolute top-2.5 right-2.5 grid size-7 cursor-pointer place-items-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/25"
        aria-label="Dismiss notice"
        onClick={onDismiss}
      >
        <X className="size-4" aria-hidden="true" />
      </button>

      <div className="flex items-start gap-3 pr-7">
        <ShieldAlert
          className="mt-0.5 size-4 shrink-0 text-warning"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <p className="text-md font-semibold">{copy.title}</p>
          <p id={bodyId} className="mt-1 text-sm text-muted-foreground">
            {copy.body}
          </p>
        </div>
      </div>

      <div className="mt-4 flex flex-wrap items-center gap-2">
        <Button type="button" size="sm" onClick={onAllow}>
          Allow…
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={onOpenSettings}
        >
          Open Settings → Permissions
        </Button>
      </div>
    </aside>
  );
}
