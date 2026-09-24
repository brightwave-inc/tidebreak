import { useEffect, useRef } from "react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { PermissionAsk } from "./computerUsePermissionAsk";
import {
  computerUsePermissionHost,
  type ComputerUsePermissionHost,
} from "./computerUsePermissions";
import { ComputerUsePermissionRows } from "./settings/ComputerUsePermissionRows";
import { ComputerUseRestartOffer } from "./settings/ComputerUseRestartOffer";
import { SettingsError } from "./settings/primitives";
import { useComputerUsePermissions } from "./settings/useComputerUsePermissions";

/** The ask's title and lead, by what it is for and who started it. */
export function permissionAskCopy(ask: PermissionAsk): {
  title: string;
  description: string;
} {
  if (ask.subject === "browser") {
    return {
      title: "Allow Tidebreak to control your browser",
      description: ask.forTask
        ? "A task needs two macOS permissions to see and use your browser window."
        : "Tidebreak needs two macOS permissions to see and use your browser window.",
    };
  }
  return {
    title: "Allow Tidebreak to use apps on this Mac",
    description: ask.forTask
      ? "A task needs two macOS permissions to see and use your other apps."
      : "Tidebreak needs two macOS permissions to see and use your other apps.",
  };
}

/**
 * The macOS permission ask, at the moment computer use needs it.
 *
 * It opens the first time a task needs Accessibility or Screen Recording, and
 * again whenever the person acts to turn computer use on, such as allowing a
 * task to use an app. It never opens at launch. Each permission says in one
 * sentence what it enables.
 *
 * Not now takes focus, so Enter never raises the macOS prompts by accident.
 * Settings → Permissions is where the person finds the permissions again.
 * Screen Recording turned on while Tidebreak runs applies only after a
 * restart, so the ask offers one.
 */
export function ComputerUsePermissionDialog({
  ask,
  host = computerUsePermissionHost,
  onClose,
  onOpenSettings,
}: {
  ask: PermissionAsk;
  host?: ComputerUsePermissionHost;
  /** Not now, Escape, or Done once both permissions are allowed. */
  onClose: (outcome: "not_now" | "ready") => void;
  /** Go to Settings → Permissions instead. */
  onOpenSettings?: () => void;
}) {
  const {
    permissions,
    missing,
    error,
    requesting,
    opening,
    busy,
    restartSuggested,
    restarting,
    request,
    openSettings,
    restart,
    // No Refresh button here: the hook re-reads when the window regains
    // focus, which is exactly the trip out to System Settings and back.
  } = useComputerUsePermissions(host, { refreshable: false });
  const ready = permissions !== null && !missing;
  const notNow = useRef<HTMLButtonElement>(null);
  const done = useRef<HTMLButtonElement>(null);
  const copy = permissionAskCopy(ask);

  // Not now leaves once both permissions arrive, so focus moves to Done
  // rather than falling back to the page behind the dialog.
  useEffect(() => {
    if (ready) done.current?.focus();
  }, [ready]);

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next) onClose(ready ? "ready" : "not_now");
      }}
    >
      <DialogContent
        // Radix focuses the content box itself, which would otherwise draw a
        // ring around the whole dialog; the buttons carry the real indicator.
        className="max-w-xl outline-none"
        withCloseButton={false}
        // Land on the answer that changes nothing. Allow raises the macOS
        // prompts, so it must never be one keypress away.
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          (notNow.current ?? done.current)?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>{copy.title}</DialogTitle>
          <DialogDescription>{copy.description}</DialogDescription>
        </DialogHeader>
        {permissions ? (
          <ComputerUsePermissionRows
            permissions={permissions}
            subject={ask.subject}
            disabled={busy}
            opening={opening}
            onOpenSettings={(pane) => void openSettings(pane)}
          />
        ) : (
          <p role="status" className="text-sm text-muted-foreground">
            Checking macOS permissions…
          </p>
        )}
        {restartSuggested && (
          <ComputerUseRestartOffer
            restarting={restarting}
            disabled={busy && !restarting}
            onRestart={() => void restart()}
          />
        )}
        {error && <SettingsError>{error}</SettingsError>}
        <p className="text-sm text-muted-foreground">
          {ready ? (
            "macOS permissions are ready. Each task still asks before it uses an app."
          ) : (
            <>
              You can allow them later in{" "}
              {onOpenSettings ? (
                <Button
                  type="button"
                  variant="link"
                  // The 2xs size sets a height the merge can replace.
                  size="2xs"
                  className="inline h-auto border-0 p-0 align-baseline text-sm"
                  onClick={onOpenSettings}
                >
                  Settings → Permissions
                </Button>
              ) : (
                "Settings → Permissions"
              )}
              .
            </>
          )}
        </p>
        <DialogFooter>
          {ready ? (
            <Button ref={done} onClick={() => onClose("ready")}>
              Done
            </Button>
          ) : (
            <>
              <Button
                ref={notNow}
                variant="outline"
                onClick={() => onClose("not_now")}
              >
                Not now
              </Button>
              <Button disabled={busy} onClick={() => void request()}>
                {requesting ? "Requesting…" : "Allow"}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
