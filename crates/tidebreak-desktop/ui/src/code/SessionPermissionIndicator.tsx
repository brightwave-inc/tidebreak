import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

import type { PermissionMode } from "../api/types";
import { permissionModeOption } from "../PermissionModeMenu";
import { FOCUS_RING } from "./interactive";
import { PERMISSION_MODE_LABELS, sessionPermissionModeTooltip } from "./labels";

/**
 * The permission mode a code session is running under, in the workspace
 * header.
 *
 * Create picks the most autonomous posture the engine honors (decision 0039,
 * amended 2026-08-18), so outside the composer the mode was invisible and a
 * reader could not tell an asking session from an allow-all one. The chip
 * states it wherever the session is.
 *
 * It reads as metadata, not as a warning: same muted size and weight as the
 * lifecycle text beside it. A higher posture is a normal operating mode, not
 * an alarm, and the icon carries the recognition.
 */
export function SessionPermissionIndicator({ mode }: { mode: PermissionMode }) {
  const Icon = permissionModeOption(mode).icon;
  return (
    <WithTooltip label={sessionPermissionModeTooltip(mode)}>
      {/*
       * The tooltip is the only place the posture is spelled out, and a span
       * cannot be tabbed to, so the tab stop keeps it reachable without a
       * pointer.
       */}
      <span
        data-testid="session-permission-indicator"
        tabIndex={0}
        className={cn(
          "inline-flex items-center gap-1 rounded-sm text-xs text-muted-foreground",
          FOCUS_RING,
        )}
      >
        <Icon className="size-3 shrink-0" aria-hidden="true" />
        <span className="truncate">{PERMISSION_MODE_LABELS[mode]}</span>
      </span>
    </WithTooltip>
  );
}
