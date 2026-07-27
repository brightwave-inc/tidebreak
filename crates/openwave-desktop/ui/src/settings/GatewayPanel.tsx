import { useCallback, useEffect, useRef, useState } from "react";
import { ExternalLink, LogOut, RefreshCw } from "lucide-react";
import type { ApiClient, GatewayStatus } from "../api";
import { openExternal } from "../host";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
        <label className="flex items-center gap-2 text-sm font-medium">
          <input
            type="checkbox"
            checked={status.enabled}
            disabled={working}
            onChange={(event) => void save(event.target.checked)}
          />
          Use Model Gateway
        </label>

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

      <p className="text-sm leading-relaxed text-muted-foreground">
        Sign-in happens in your browser against the gateway itself; OpenWave
        never sees your identity provider credentials. Tokens are stored in the
        system keychain and revoked at the gateway when you disconnect.
      </p>
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}
