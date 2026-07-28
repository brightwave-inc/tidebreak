import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { ExternalLink, LogOut, RefreshCw } from "lucide-react";
import type { ApiClient, GatewayApps, GatewayStatus } from "../api";
import { openExternal } from "../host";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";

const SIGN_IN_POLL_MS = 2_000;

/** Native opener first; `window.open` only works in a plain browser tab. */
async function openSignInPage(url: string) {
  if (!(await openExternal(url).catch(() => false))) {
    window.open(url, "_blank", "noreferrer,noopener");
  }
}

/**
 * The Model Gateway connection: one toggle, one URL, one sign-in.
 *
 * Authentication is the gateway's own OAuth flow in the system browser —
 * OpenWave never sees a password or IdP credential, only the gateway's
 * rotating tokens, which live in the keychain. Signed in, the entitled
 * models sync into the picker; signed out, the app is exactly the local
 * bring-your-own-key product it was before.
 */
export function GatewayPanel({
  client,
  onChanged,
}: {
  client: ApiClient;
  onChanged: () => void;
}) {
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [apps, setApps] = useState<GatewayApps | null>(null);
  const [baseUrl, setBaseUrl] = useState("");
  const [dirty, setDirty] = useState(false);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Refs, not state reads, inside `reload`: transition detection must not
  // live in a state updater (updaters are pure and StrictMode double-invokes
  // them), and the poll must not refill a field the user is editing.
  const signedInRef = useRef(false);
  const dirtyRef = useRef(false);

  const reload = useCallback(async () => {
    const next = await client.getGatewayStatus();
    // Entitlements changed while we watched: refresh the model picker.
    if (!signedInRef.current && next.signed_in) onChanged();
    signedInRef.current = next.signed_in;
    setStatus(next);
    setBaseUrl((current) =>
      current === "" && !dirtyRef.current ? (next.base_url ?? "") : current,
    );
    return next;
  }, [client, onChanged]);

  useEffect(() => {
    reload().catch((err) => setError(String(err)));
  }, [reload]);

  // Entitled apps are never cached server-side (a revoked grant disappears on
  // the next request), so fetch them fresh whenever the signed-in state turns
  // on. A fetch failure only hides the section; models keep working.
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

  async function save(enabled: boolean) {
    await run(async () => {
      await client.putProvider("model_gateway", {
        enabled,
        base_url: baseUrl.trim() === "" ? null : baseUrl.trim(),
      });
      setDirty(false);
      dirtyRef.current = false;
      onChanged();
      toast.success("Saved gateway settings");
    });
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

  return (
    <SettingsPanel
      title="Model Gateway"
      description="Sign in to a model-gateway deployment to use the models and governed tools you are entitled to. Off, OpenWave stays fully local."
      busy={working}
    >
      <SettingsSection>
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1">
            <p className="text-sm font-bold">Use Model Gateway</p>
            <p className="text-xs text-muted-foreground">
              Route models and governed tools through a signed-in gateway. Off,
              OpenWave stays fully local.
            </p>
          </div>
          <Switch
            aria-label="Use Model Gateway"
            checked={status.enabled}
            disabled={working}
            onCheckedChange={(checked) => void save(checked)}
          />
        </div>

        <SettingsField
          label="Gateway URL"
          hint="The deployment's base URL, e.g. http://127.0.0.1:28081 for a local dev gateway."
        >
          <Input
            value={baseUrl}
            disabled={working}
            autoComplete="off"
            spellCheck={false}
            placeholder="https://gateway.example"
            onChange={(event) => {
              setBaseUrl(event.target.value);
              setDirty(true);
              dirtyRef.current = true;
            }}
          />
        </SettingsField>
        {dirty && (
          <Button
            type="button"
            disabled={working}
            onClick={() => void save(status.enabled)}
          >
            Save gateway URL
          </Button>
        )}
      </SettingsSection>

      <SettingsSection title="Connection">
        {status.signed_in ? (
          <>
            <div className="web-search-state is-ready" role="status">
              <strong>Signed in</strong>
              <span>
                {status.account_hint ?? "Connected"} ·{" "}
                {status.model_count === 1
                  ? "1 model entitled"
                  : `${status.model_count} models entitled`}
              </span>
            </div>
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
                    toast.success("Refreshed entitled models");
                  })
                }
              >
                <RefreshCw size={14} />
                Refresh models
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
            <div className="web-search-state is-not-configured" role="status">
              <strong>Not signed in</strong>
              <span>
                {status.configured
                  ? "Connect to sign in with your browser."
                  : "Save the gateway URL, then connect."}
              </span>
            </div>
            {status.sign_in.state === "failed" && (
              <SettingsError>{status.sign_in.message}</SettingsError>
            )}
            {pendingUrl ? (
              <p className="text-sm">
                Waiting for the browser…{" "}
                <a
                  className="underline"
                  href={pendingUrl}
                  target="_blank"
                  rel="noreferrer noopener"
                  onClick={(event) => {
                    // The webview swallows target="_blank"; route through the
                    // native opener and keep the href for hover/copy.
                    event.preventDefault();
                    void openSignInPage(pendingUrl);
                  }}
                >
                  Open the sign-in page again
                </a>
              </p>
            ) : (
              <Button
                type="button"
                disabled={working || !status.configured || dirty}
                onClick={() =>
                  void run(async () => {
                    const started = await client.gatewaySignIn();
                    await openSignInPage(started.authorization_url);
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

      {status.signed_in && apps?.supported && (
        <SettingsSection title="Connected apps">
          {apps.apps.length === 0 ? (
            <p className="text-muted-foreground text-sm">
              No connected apps are granted to your teams yet.
            </p>
          ) : (
            <ul className="flex flex-col gap-2">
              {apps.apps.map((app) => (
                <li key={app.id} className="rounded-md border px-3 py-2 text-sm">
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
                </li>
              ))}
            </ul>
          )}
        </SettingsSection>
      )}

      <p className="text-sm leading-relaxed text-muted-foreground">
        Sign-in happens in your browser against the gateway itself; OpenWave
        never sees your identity provider credentials. Tokens are stored in the
        system keychain and revoked at the gateway when you disconnect.
      </p>
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}
