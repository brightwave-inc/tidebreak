import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import type { PermissionAskSubject } from "@/computerUsePermissionAsk";
import type {
  ComputerUsePermissionPane,
  ComputerUsePermissionStatus,
} from "@/computerUsePermissions";

/** A grant's name as System Settings shows it. */
export const PERMISSION_LABELS: Record<ComputerUsePermissionPane, string> = {
  accessibility: "Accessibility",
  screen_recording: "Screen Recording",
};

/**
 * What each grant lets Tidebreak do, in one sentence, worded for what the
 * person is setting up: other apps, or a browser's own window.
 */
const PERMISSION_PURPOSE: Record<
  PermissionAskSubject,
  Record<ComputerUsePermissionPane, string>
> = {
  apps: {
    accessibility: "Lets Tidebreak read other apps' controls, click, and type.",
    screen_recording: "Lets Tidebreak take screenshots of apps and displays.",
  },
  browser: {
    accessibility:
      "Lets Tidebreak read your browser's controls, click, and type in it.",
    screen_recording: "Lets Tidebreak take screenshots of your browser window.",
  },
};

/** The two macOS grants computer use needs, in the order System Settings lists them. */
const PANES = [
  "accessibility",
  "screen_recording",
] as const satisfies readonly ComputerUsePermissionPane[];

/**
 * One row per macOS grant: what it allows, whether it is held, and the way to
 * the System Settings pane that changes it.
 *
 * Settings and the ask a task raises both render this, so the two never
 * drift on what a grant is called or what it lets Tidebreak do.
 */
export function ComputerUsePermissionRows({
  permissions,
  subject = "apps",
  disabled,
  opening,
  onOpenSettings,
}: {
  permissions: Extract<ComputerUsePermissionStatus, { status: "available" }>;
  subject?: PermissionAskSubject;
  disabled: boolean;
  opening: ComputerUsePermissionPane | null;
  onOpenSettings: (pane: ComputerUsePermissionPane) => void;
}) {
  return (
    <div className="divide-y divide-border">
      {PANES.map((pane) => {
        const label = PERMISSION_LABELS[pane];
        const granted =
          pane === "accessibility"
            ? permissions.accessibility
            : permissions.screenRecording;
        return (
          <div
            key={pane}
            className="flex flex-wrap items-center justify-between gap-3 py-3"
          >
            <div className="min-w-0 flex-1 basis-48">
              <p className="text-sm font-medium">{label}</p>
              <p className="text-sm text-muted-foreground">
                {PERMISSION_PURPOSE[subject][pane]}
              </p>
            </div>
            <div className="flex items-center gap-3">
              <Badge variant={granted ? "success" : "warning"}>
                {granted ? "Allowed" : "Not allowed"}
              </Badge>
              {/* System Settings, not Tidebreak's: the grant lives there. */}
              <Button
                variant="outline"
                size="sm"
                disabled={disabled}
                aria-label={`Open ${label} in System Settings`}
                onClick={() => onOpenSettings(pane)}
              >
                {opening === pane ? "Opening…" : "Open System Settings"}
              </Button>
            </div>
          </div>
        );
      })}
    </div>
  );
}
