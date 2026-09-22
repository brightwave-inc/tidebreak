import { Button } from "@/components/ui/button";
import {
  computerUsePermissionHost,
  type ComputerUsePermissionHost,
} from "@/computerUsePermissions";
import { ComputerUsePermissionRows } from "./ComputerUsePermissionRows";
import { SettingsError, SettingsSection } from "./primitives";
import { useComputerUsePermissions } from "./useComputerUsePermissions";

export function ComputerUsePermissionsSection({
  host = computerUsePermissionHost,
}: {
  host?: ComputerUsePermissionHost;
}) {
  const {
    availability,
    status,
    permissions,
    missing,
    error,
    loading,
    requesting,
    opening,
    busy,
    refresh,
    request,
    openSettings,
  } = useComputerUsePermissions(host);

  return (
    <SettingsSection
      title="Computer use on this Mac"
      description="Allow Tidebreak to read, capture, and control other apps. You still choose which apps each task may use."
    >
      {availability !== "local" ? (
        <p className="text-sm text-muted-foreground">
          {availability === "remote"
            ? "Switch to this Mac to set up its computer-use permissions. These settings do not grant access to a remote machine."
            : "Open Tidebreak on your Mac to set up computer use."}
        </p>
      ) : (
        <>
          {loading && !permissions && (
            <p role="status" className="text-sm text-muted-foreground">
              Checking macOS permissions…
            </p>
          )}
          {status?.status === "unsupported" && (
            <p className="text-sm text-muted-foreground">
              Native app control requires the macOS desktop app.
            </p>
          )}
          {permissions && (
            <>
              <p className="text-sm text-muted-foreground break-words">
                In System Settings, enable permissions for {permissions.appName}
                .
                {permissions.appIdentifier && (
                  <span className="mt-1 block break-all font-mono text-xs">
                    {permissions.appIdentifier}
                  </span>
                )}
              </p>
              <ComputerUsePermissionRows
                permissions={permissions}
                disabled={busy}
                opening={opening}
                onOpenSettings={(pane) => void openSettings(pane)}
              />
              <p role="status" className="text-sm text-muted-foreground">
                {missing
                  ? "Enable the missing permissions, then return here. If macOS asks you to quit and reopen Tidebreak, do that before retrying the task."
                  : "macOS permissions are ready. Each task still asks for access to the apps it needs."}
              </p>
            </>
          )}
          {error && <SettingsError>{error}</SettingsError>}
          {status?.status !== "unsupported" && (
            <div className="flex flex-wrap gap-2">
              {missing && (
                <Button
                  size="sm"
                  disabled={busy || loading}
                  onClick={() => void request()}
                >
                  {requesting
                    ? "Requesting permissions…"
                    : "Request macOS permissions"}
                </Button>
              )}
              <Button
                variant="outline"
                size="sm"
                disabled={busy || loading}
                onClick={() => void refresh()}
              >
                {loading ? "Checking…" : "Refresh"}
              </Button>
            </div>
          )}
        </>
      )}
    </SettingsSection>
  );
}
