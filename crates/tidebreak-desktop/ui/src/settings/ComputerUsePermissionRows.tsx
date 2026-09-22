import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import type {
  ComputerUsePermissionPane,
  ComputerUsePermissionStatus,
} from "@/computerUsePermissions";

/** The two macOS grants computer use needs, in the order System Settings lists them. */
const GRANTS = [
  ["accessibility", "Accessibility", "Read app controls, click, and type."],
  [
    "screen_recording",
    "Screen Recording",
    "Capture screenshots of apps and displays.",
  ],
] as const satisfies readonly (readonly [
  ComputerUsePermissionPane,
  string,
  string,
])[];

/**
 * One row per macOS grant: what it allows, whether it is held, and the way to
 * the System Settings pane that changes it.
 *
 * Settings and the first-run setup dialog both render this, so the two never
 * drift on what a grant is called or what it lets Tidebreak do.
 */
export function ComputerUsePermissionRows({
  permissions,
  disabled,
  opening,
  onOpenSettings,
}: {
  permissions: Extract<ComputerUsePermissionStatus, { status: "available" }>;
  disabled: boolean;
  opening: ComputerUsePermissionPane | null;
  onOpenSettings: (pane: ComputerUsePermissionPane) => void;
}) {
  return (
    <div className="divide-y divide-border">
      {GRANTS.map(([pane, label, description]) => {
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
              <p className="text-sm text-muted-foreground">{description}</p>
            </div>
            <div className="flex items-center gap-3">
              <Badge variant={granted ? "success" : "warning"}>
                {granted ? "Allowed" : "Not allowed"}
              </Badge>
              <Button
                variant="outline"
                size="sm"
                disabled={disabled}
                aria-label={`Open ${label} settings`}
                onClick={() => onOpenSettings(pane)}
              >
                {opening === pane ? "Opening…" : "Open settings"}
              </Button>
            </div>
          </div>
        );
      })}
    </div>
  );
}
