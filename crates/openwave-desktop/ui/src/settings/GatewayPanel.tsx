import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { ExternalLink, LogOut, RefreshCw } from "lucide-react";
import type {
  ApiClient,
  GatewayApps,
  GatewayStatus,
  McpServerDefinition,
  McpServerInfo,
} from "../api";
import { openSignInPage } from "../openSignInPage";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

const SIGN_IN_POLL_MS = 2_000;
/** Mount health lives in the local MCP supervisor, so a modest refresh while
 * the panel is visible keeps the health lines honest without gateway load. */
const MOUNT_REFRESH_MS = 15_000;
const DEFAULT_MOUNT_TIMEOUT_MS = 60_000;
/** Server names cap at 32 bytes (the MCP tool namespace); endpoint slugs go
 * to 127, so the mount name is derived, not the slug itself. Mount identity
 * is always the `gateway_endpoint` field, never the name. */
const MAX_NAMESPACE_BYTES = 32;

function definitionOf(server: McpServerInfo): McpServerDefinition {
  const { health: _, tool_count: __, diagnostic: ___, ...value } = server;
  return value;
}

/** A valid, unused namespace for a mount: the slug, truncated to the name
 * limit and de-duplicated against every configured server. */
function mountName(slug: string, taken: ReadonlySet<string>): string {
  const base = slug.slice(0, MAX_NAMESPACE_BYTES);
  if (!taken.has(base)) return base;
  for (let n = 2; ; n += 1) {
    const suffix = `_${n}`;
    const candidate =
      base.slice(0, MAX_NAMESPACE_BYTES - suffix.length) + suffix;
    if (!taken.has(candidate)) return candidate;
  }
}

/** Sentence-shaped status for a mount row; diagnostics already are one. */
function mountStatus(mounted: McpServerInfo): string {
  if (mounted.health === "healthy") {
    return `${mounted.tool_count} tool${mounted.tool_count === 1 ? "" : "s"} available to new turns.`;
  }
  if (mounted.diagnostic) return mounted.diagnostic;
  switch (mounted.health) {
    case "initializing":
    case "reconnecting":
      return "Connecting…";
    case "disabled":
      return "Disabled in MCP servers settings.";
    default:
      return "Needs attention. See MCP servers settings.";
  }
}

/** A fresh gateway mount: everything comes from the session except the name,
 * which doubles as the tool namespace. */
function mountDefinition(slug: string, name: string): McpServerDefinition {
  return {
    name,
    command: null,
    args: [],
    env: {},
    env_from: [],
    cwd: null,
    url: null,
    bearer_token_env: null,
    gateway_endpoint: slug,
    request_timeout_ms: DEFAULT_MOUNT_TIMEOUT_MS,
    enabled: true,
  };
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
  const [mcpServers, setMcpServers] = useState<McpServerInfo[] | null>(null);
  const [mcpError, setMcpError] = useState<string | null>(null);
  // Bumped by the Retry affordance; re-runs the mount-list effect immediately
  // and restarts its cadence.
  const [mountsRefreshNonce, setMountsRefreshNonce] = useState(0);
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

  // Mount state lives in the MCP configuration; load it alongside the apps so
  // each endpoint's toggle reflects what is actually configured — and keep
  // re-reading it while the panel is visible, so a mount that degrades after
  // the first read doesn't keep a stale healthy line. A failed read keeps the
  // last-known rows and surfaces a retryable error instead of silently
  // disabling every toggle.
  useEffect(() => {
    if (!status?.signed_in) {
      setMcpServers(null);
      setMcpError(null);
      return;
    }
    let cancelled = false;
    const refresh = async () => {
      try {
        const next = await client.listMcpServers();
        if (!cancelled) {
          setMcpServers(next.servers);
          setMcpError(null);
        }
      } catch (err) {
        if (!cancelled) setMcpError(String(err));
      }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), MOUNT_REFRESH_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [client, status?.signed_in, mountsRefreshNonce]);

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

  async function setMounted(slug: string, mounted: boolean) {
    await run(async () => {
      // Rebuild from the live configuration, not the panel cache, so a mount
      // toggled here never drops a server edited elsewhere in the meantime.
      const current = (await client.listMcpServers()).servers.map(definitionOf);
      const without = current.filter(
        (server) => server.gateway_endpoint !== slug,
      );
      const taken = new Set(without.map((server) => server.name));
      const next = mounted
        ? [...without, mountDefinition(slug, mountName(slug, taken))]
        : without;
      const result = await client.putMcpServers(next);
      setMcpServers(result.servers);
      setMcpError(null);
      toast.success(mounted ? `Mounted ${slug}` : `Unmounted ${slug}`);
    });
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
  const entitledSlugs = new Set(
    apps?.apps.flatMap((app) => app.mcp_endpoint_slugs) ?? [],
  );
  // Rows are the union of what's entitled and what's configured: a mount
  // whose grant was revoked must keep its row (and unmount toggle) here,
  // not silently drop to being visible only in the MCP panel while
  // supervision retries it.
  const endpointSlugs = [
    ...new Set([
      ...entitledSlugs,
      ...(mcpServers ?? [])
        .map((server) => server.gateway_endpoint)
        .filter((slug): slug is string => slug !== null),
    ]),
  ];

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
            <SettingsStatus
              tone="not-configured"
              label="Not signed in"
              description={
                status.configured
                  ? "Connect to sign in with your browser."
                  : "Save the gateway URL, then connect."
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

          {(endpointSlugs.length > 0 || mcpError !== null) && (
            <div className="flex flex-col gap-2">
              <div>
                <p className="text-sm font-bold">MCP endpoints</p>
                <p className="text-xs text-muted-foreground">
                  Mounted endpoints connect with your gateway session — no
                  tokens to copy, and they reconnect after you sign back in.
                </p>
              </div>
              {mcpError !== null && (
                <div className="flex items-center justify-between gap-4">
                  <SettingsError>
                    Couldn't read the MCP server list: {mcpError}
                  </SettingsError>
                  <Button
                    type="button"
                    variant="outline"
                    disabled={working}
                    onClick={() => setMountsRefreshNonce((nonce) => nonce + 1)}
                  >
                    <RefreshCw size={14} />
                    Retry
                  </Button>
                </div>
              )}
              <ul className="flex flex-col gap-2">
                {endpointSlugs.map((slug) => {
                  const mounted = mcpServers?.find(
                    (server) => server.gateway_endpoint === slug,
                  );
                  return (
                    <li
                      key={slug}
                      className="flex items-center justify-between gap-4 rounded-md border px-3 py-2 text-sm"
                    >
                      <div className="min-w-0 flex-1">
                        <code className="font-medium">{slug}</code>
                        {!entitledSlugs.has(slug) ? (
                          <p className="text-muted-foreground text-xs">
                            No longer granted to your teams. Switch off to
                            unmount it.
                          </p>
                        ) : (
                          mounted && (
                            <p className="text-muted-foreground text-xs">
                              {mountStatus(mounted)}
                            </p>
                          )
                        )}
                      </div>
                      <Switch
                        aria-label={`Mount ${slug}`}
                        checked={mounted !== undefined}
                        disabled={working || mcpServers === null}
                        onCheckedChange={(checked) =>
                          void setMounted(slug, checked)
                        }
                      />
                    </li>
                  );
                })}
              </ul>
            </div>
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
