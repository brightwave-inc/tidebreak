import { useRef } from "react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  computerUsePermissionHost,
  type ComputerUsePermissionHost,
} from "./computerUsePermissions";
import { ComputerUsePermissionRows } from "./settings/ComputerUsePermissionRows";
import { SettingsError } from "./settings/primitives";
import { useComputerUsePermissions } from "./settings/useComputerUsePermissions";

/**
 * The one-time macOS setup ask, shown the first time Tidebreak opens on a Mac
 * that has not granted Accessibility and Screen Recording.
 *
 * Both grants are hard requirements for computer use, and macOS only surfaces
 * them when something asks. Left to a tool call, the ask lands mid-task — and
 * a Screen Recording grant reaches a process that is already running only
 * after it is restarted, so the person loses the session they were in. Asking
 * on the first launch spends a moment when nothing is underway.
 *
 * It asks once. Whether the person grants, declines, or closes it, the dialog
 * does not come back; Settings → Permissions is the way in afterwards.
 */
export function ComputerUseSetupDialog({
  open,
  host = computerUsePermissionHost,
  onDone,
}: {
  open: boolean;
  host?: ComputerUsePermissionHost;
  onDone: () => void;
}) {
  const {
    permissions,
    missing,
    error,
    requesting,
    opening,
    busy,
    request,
    openSettings,
    // No Refresh button here: the hook re-reads when the window regains
    // focus, which is exactly the trip out to System Settings and back.
  } = useComputerUsePermissions(host, { refreshable: false });
  const ready = permissions !== null && !missing;
  const confirm = useRef<HTMLButtonElement>(null);

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) onDone();
      }}
    >
      <DialogContent
        // Radix focuses the content box itself, which would otherwise draw a
        // ring around the whole dialog; the buttons carry the real indicator.
        className="max-w-xl outline-none"
        withCloseButton={false}
        // Land on the button that grants, not on the one that declines and
        // not on the content box, whose focus ring would frame the dialog.
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          confirm.current?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>Allow Tidebreak to use this Mac</DialogTitle>
          <DialogDescription>
            To let Tidebreak see and control your other apps, macOS needs two
            permissions. Granting them now keeps a task from stopping partway to
            ask.
          </DialogDescription>
        </DialogHeader>
        {permissions ? (
          <ComputerUsePermissionRows
            permissions={permissions}
            disabled={busy}
            opening={opening}
            onOpenSettings={(pane) => void openSettings(pane)}
          />
        ) : (
          <p role="status" className="text-sm text-muted-foreground">
            Checking macOS permissions…
          </p>
        )}
        {error && <SettingsError>{error}</SettingsError>}
        <p className="text-sm text-muted-foreground">
          {ready
            ? "macOS permissions are ready. Each task still asks for access to the apps it needs."
            : "You can change this later in Settings → Permissions."}
        </p>
        <DialogFooter>
          {!ready && (
            <Button variant="ghost" onClick={onDone}>
              Not now
            </Button>
          )}
          <Button
            ref={confirm}
            disabled={busy}
            onClick={ready ? onDone : () => void request()}
          >
            {ready ? "Done" : requesting ? "Requesting…" : "Allow"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
