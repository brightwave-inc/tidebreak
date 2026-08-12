import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { ExternalLink, LogOut, PlugZap, RefreshCw } from "lucide-react";
import type { ApiClient, GatewayApps, GatewayStatus } from "../api";
import { openInBrowser } from "../openInBrowser";
import { Button } from "@/components/ui/button";
import {
  SettingsError,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
  usedByLabel,
} from "./primitives";

const SIGN_IN_POLL_MS = 2_000;

/**
 * The Model Gateway section.
 *
 * Policy is the only gateway source: a profile connects through the
 * gateway's own page (deep-link pairing) or the organization's device
 * management, never from settings — there is no URL field and no enable
 * toggle in any state. Unmanaged profiles render a signpost at that flow;
 * managed profiles get the slim identity panel: who is signed in, the
 * read-only gateway origin from policy, sign in/out, and an explicit
 * gateway sync (models and MCP endpoint mounts together).
 */
export function GatewayPanel({
  client,
  managed,
  gatewayUrl,
  onChanged,
  onOpenConnectedApps,
}: {
  client: ApiClient;
  /** Whether the resolved policy manages this profile. */
  managed: boolean;
  /** The policy's locked gateway origin, shown read-only. */
  gatewayUrl: string | null;
  onChanged: () => void;
  /** Navigates to the Connected apps page, whose MCP section is where
   * entitled endpoints are mounted. Mounting lives beside the health of what
   * is mounted, so this panel points at it rather than carrying its own
   * toggles. */
  onOpenConnectedApps: () => void;
}) {
  if (!managed) {
    // Deep links and stale history entries still resolve here even though
    // the rail hides the section; say plainly how connecting works now.
    return (
      <SettingsPanel
        title="Model Gateway"
        description="This profile is not connected to a model gateway."
      >
        <p className="text-sm leading-relaxed text-muted-foreground">
          Connecting happens from your gateway&apos;s own page — open it in
          your browser and choose Connect — or through your
          organization&apos;s device management. There is nothing to
          configure here; until then, Tidebreak stays fully local with your
          own provider keys.
        </p>
      </SettingsPanel>
    );
  }
  return (
    <ManagedGatewayPanel
      client={client}
      gatewayUrl={gatewayUrl}
      onChanged={onChanged}
      onOpenConnectedApps={onOpenConnectedApps}
    />
  );
}

/**
 * Authentication is the gateway's own OAuth flow in the system browser —
 * Tidebreak never sees a password or IdP credential, only the gateway's
 * rotating tokens, which live in the keychain.
 */
function ManagedGatewayPanel({
  client,
  gatewayUrl,
  onChanged,
  onOpenConnectedApps,
}: {
  client: ApiClient;
  gatewayUrl: string | null;
  onChanged: () => void;
  onOpenConnectedApps: () => void;
}) {
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [apps, setApps] = useState<GatewayApps | null>(null);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // A ref, not a state read, inside `reload`: transition detection must not
  // live in a state updater (updaters are pure and StrictMode double-invokes
  // them).
  const signedInRef = useRef(false);

  const reload = useCallback(async () => {
    const next = await client.getGatewayStatus();
    // Entitlements changed while we watched: refresh the model picker.
    if (!signedInRef.current && next.signed_in) onChanged();
    signedInRef.current = next.signed_in;
    setStatus(next);
    return next;
  }, [client, onChanged]);

  useEffect(() => {
    reload().catch((err) => setError(String(err)));
  }, [reload]);

  // Entitled apps are never cached server-side (a revoked grant disappears on
  // the next request), so fetch them fresh whenever the signed-in state turns
  // on. A failure leaves the list absent rather than asserting anything about
  // what is granted.
  useEffect(() => {
    if (!status?.signed_in) {
      setApps(null);
      return;
    }
    let cancelled = false;
    client
      .getGatewayApps()
      .then((next) => {
        if (!cancelled) setApps(next);
      })
      .catch(() => {
        if (!cancelled) setApps(null);
      });
    return () => {
      cancelled = true;
    };
  }, [client, status?.signed_in]);

  // While the browser flow is pending, watch for its outcome.
  useEffect(() => {
    if (status?.sign_in.state !== "pending") return;
    const timer = window.setInterval(() => {
      void reload().catch(() => undefined);
    }, SIGN_IN_POLL_MS);
    return () => window.clearInterval(timer);
  }, [status?.sign_in.state, reload]);

  async function run(action: () => Promise<unknown>) {
    setWorking(true);
    setError(null);
    try {
      await action();
      await reload();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  if (!status) {
    return (
      <SettingsPanel title="Model Gateway" description="Loading…" busy>
        {error && <SettingsError>{error}</SettingsError>}
      </SettingsPanel>
    );
  }

  const pendingUrl =
    status.sign_in.state === "pending" ? status.sign_in.authorization_url : null;
  // The policy names the deployment; the status echoes it. Prefer the policy
  // (it is what the profile is locked to) and fall back to the echo.
  const origin = gatewayUrl ?? status.base_url ?? null;
  // The one route to mounting, shown whenever signed in: a gateway without
  // the apps surface still mounts endpoints by slug from the Connected apps
  // page's MCP section.
  const mountSignpost = (
    <Button
      type="button"
      variant="outline"
      className="self-start"
      onClick={onOpenConnectedApps}
    >
      <PlugZap size={14} />
      Mount endpoints in Connected apps
    </Button>
  );

  return (
    <SettingsPanel
      title="Model Gateway"
      description="This profile is managed by your organization's model gateway: models and governed tools come from the deployment below."
      busy={working}
    >
      <SettingsSection title="Gateway">
        <p className="text-sm">
          <code className="font-medium">{origin ?? "—"}</code>
        </p>
        <p className="text-xs text-muted-foreground">
          Set by your organization&apos;s policy and not editable here.
        </p>
      </SettingsSection>

      <SettingsSection title="Connection">
        {status.signed_in ? (
          <>
            <SettingsStatus
              tone="ready"
              label="Signed in"
              description={`${status.account_hint ?? "Connected"} · ${
                status.model_count === 1
                  ? "1 model entitled"
                  : `${status.model_count} models entitled`
              }`}
            />
            {status.installation_id && (
              <p className="text-muted-foreground text-xs">
                Installation {status.installation_id}
              </p>
            )}
            <div className="flex flex-wrap gap-2">
              <Button
                type="button"
                variant="outline"
                disabled={working}
                onClick={() =>
                  void run(async () => {
                    await client.syncGatewayModels();
                    onChanged();
                    toast.success("Synced models and MCP endpoints from the gateway");
                  })
                }
              >
                <RefreshCw size={14} />
                Sync with gateway
              </Button>
              <Button
                type="button"
                variant="destructive"
                disabled={working}
                onClick={() =>
                  void run(async () => {
                    await client.gatewaySignOut();
                    onChanged();
                    toast.success("Disconnected from the gateway");
                  })
                }
              >
                <LogOut size={14} />
                Disconnect
              </Button>
            </div>
          </>
        ) : (
          <>
            <SettingsStatus
              tone="not-configured"
              label="Not signed in"
              description={
                origin === null
                  ? "This device's managed policy names no gateway. Contact your administrator."
                  : "Connect to sign in with your browser."
              }
            />
            {status.sign_in.state === "failed" && (
              <SettingsError>{status.sign_in.message}</SettingsError>
            )}
            {pendingUrl ? (
              <p className="text-sm">
                Waiting for the browser…{" "}
                <a
                  className="underline"
                  href={pendingUrl}
                  rel="noreferrer noopener"
                  onClick={(event) => {
                    // No target="_blank": the shell plugin's injected click
                    // handler opens such links itself without honoring
                    // preventDefault, which doubled this one. Route through
                    // the native opener and keep the href for hover/copy.
                    event.preventDefault();
                    void openInBrowser(pendingUrl);
                  }}
                >
                  Open the sign-in page again
                </a>
              </p>
            ) : (
              <Button
                type="button"
                // No origin means a misconfigured policy: there is no
                // deployment to sign in against, and the server would refuse.
                disabled={working || origin === null}
                onClick={() =>
                  void run(async () => {
                    const started = await client.gatewaySignIn();
                    await openInBrowser(started.authorization_url);
                  })
                }
              >
                <ExternalLink size={14} />
                Connect
              </Button>
            )}
          </>
        )}
      </SettingsSection>

      {status.signed_in &&
        (apps?.supported ? (
          <SettingsSection
            title="Connected apps"
            description="The apps your teams have granted this deployment. Mounting their MCP endpoints happens on the Connected apps page, beside the health of what is mounted."
          >
            {apps.apps.length === 0 ? (
              <p className="text-muted-foreground text-sm">
                No connected apps are granted to your teams yet.
              </p>
            ) : (
              <ul className="flex flex-col gap-2">
                {apps.apps.map((app) => (
                  <li
                    key={app.id}
                    className="rounded-md border px-3 py-2 text-sm"
                  >
                    <div className="flex items-center gap-2">
                      <span className="font-medium">{app.name}</span>
                      <span className="text-muted-foreground text-xs">
                        {app.app_kind}
                      </span>
                      {!app.enabled && (
                        <span className="text-muted-foreground text-xs">
                          disabled
                        </span>
                      )}
                    </div>
                    {app.mcp_endpoint_slugs.length > 0 && (
                      <p className="text-muted-foreground text-xs">
                        via {app.mcp_endpoint_slugs.join(", ")}
                      </p>
                    )}
                    {app.used_by_app_count > 0 && (
                      <p className="text-muted-foreground text-xs">
                        {usedByLabel(app.used_by_app_count)}
                      </p>
                    )}
                  </li>
                ))}
              </ul>
            )}
            {mountSignpost}
          </SettingsSection>
        ) : (
          <SettingsSection>{mountSignpost}</SettingsSection>
        ))}

      <p className="text-sm leading-relaxed text-muted-foreground">
        Sign-in happens in your browser against the gateway itself; Tidebreak
        never sees your identity provider credentials. Tokens are stored in the
        system keychain and revoked at the gateway when you disconnect.
      </p>
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}
