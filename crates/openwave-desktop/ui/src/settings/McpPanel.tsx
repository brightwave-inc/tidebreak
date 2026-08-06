import { useEffect, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Plus, RefreshCw, Trash2 } from "lucide-react";
import type {
  ApiClient,
  GatewayApps,
  McpCuration,
  McpHealth,
  McpServerDefinition,
  McpServerInfo,
} from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  SettingsError,
  SettingsField,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

const DEFAULT_TIMEOUT_MS = 60_000;
const MAX_TIMEOUT_MS = 3_600_000;
/** Mount health lives in the local MCP supervisor, so a modest refresh while
 * the section is visible keeps the health lines honest without gateway load. */
const MOUNT_REFRESH_MS = 15_000;
/** Server names cap at 32 bytes (the MCP tool namespace); endpoint slugs go
 * to 127, so the mount name is derived, not the slug itself. Mount identity
 * is always the `gateway_endpoint` field, never the name. */
const MAX_NAMESPACE_BYTES = 32;

function emptyServer(index: number): McpServerInfo {
  return {
    name: `server_${index + 1}`,
    command: "",
    args: [],
    env: [],
    env_from: [],
    cwd: null,
    url: null,
    bearer_token_env: null,
    gateway_endpoint: null,
    request_timeout_ms: DEFAULT_TIMEOUT_MS,
    enabled: true,
    plugin: null,
    health: "initializing",
    tool_count: 0,
    diagnostic: null,
    curated: null,
  };
}

type Transport = "stdio" | "http" | "gateway";

/**
 * The one place connection state is spelled: a coloured dot and a short
 * verdict. App entries and endpoint rows share it, so "healthy" can never
 * read two different ways on the same page.
 */
export function McpHealthChip({ health }: { health: McpHealth }) {
  const dot =
    health === "healthy"
      ? "text-emerald-600 dark:text-emerald-400"
      : health === "degraded"
        ? "text-destructive"
        : "text-muted-foreground";
  return (
    <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
      <span aria-hidden className={dot}>
        ●
      </span>
      {chipLabel(health)}
    </span>
  );
}

function chipLabel(health: McpHealth): string {
  switch (health) {
    case "healthy":
      return "Healthy";
    case "degraded":
      return "Needs attention";
    case "reconnecting":
      return "Reconnecting";
    case "disabled":
      return "Disabled";
    case "initializing":
      return "Connecting…";
  }
}

/**
 * The two-tier honesty label: "Tested" for a server on the curated list,
 * "Community" for everything else. A label only — both tiers mount, connect,
 * and call identically. The server decides the tier from the *saved*
 * definition, so an unsaved edit keeps the previous row's label until Save.
 */
export function McpTierChip({ curated }: { curated: McpCuration | null }) {
  const tested = curated !== null;
  return (
    <span
      className={`inline-flex items-center rounded-full border px-2 py-0.5 text-xs ${
        tested
          ? "border-emerald-600/40 text-emerald-700 dark:border-emerald-400/40 dark:text-emerald-400"
          : "text-muted-foreground"
      }`}
      title={
        tested
          ? `${curated.display_name} — exercised end to end on ${curated.tested_on}. ${curated.notes}`
          : "Not on OpenWave's tested list. It still mounts and runs; we have not driven this server ourselves."
      }
    >
      {tested ? "Tested" : "Community"}
    </span>
  );
}

function transportOf(server: McpServerInfo): Transport {
  if (server.gateway_endpoint !== null) return "gateway";
  return server.url !== null ? "http" : "stdio";
}

/** Switching transports clears the other transports' fields so a saved
 * definition can never carry more than one. */
function transportFields(transport: "stdio" | "http"): Partial<McpServerInfo> {
  return transport === "http"
    ? {
        command: null,
        args: [],
        env: [],
        env_from: [],
        cwd: null,
        url: "",
        bearer_token_env: null,
        gateway_endpoint: null,
      }
    : {
        command: "",
        url: null,
        bearer_token_env: null,
        gateway_endpoint: null,
      };
}

function definition(server: McpServerInfo): McpServerDefinition {
  const {
    health: _,
    tool_count: __,
    diagnostic: ___,
    curated: ____,
    ...value
  } = server;
  return value;
}

/**
 * A server a plugin brings with it. It is listed because the tools it mounts
 * are as real as any other server's, and read-only because its definition
 * ships inside the package: the plugin's own switch is what turns it off.
 */
function PluginServerSection({ server }: { server: McpServerInfo }) {
  return (
    <SettingsSection title={server.name}>
      <SettingsStatus
        tone={healthTone(server.health)}
        label={healthLabel(server.health)}
        description={
          server.health === "healthy"
            ? `${server.tool_count} tool${server.tool_count === 1 ? "" : "s"} available to new turns.`
            : (server.diagnostic ?? "This server is not connected.")
        }
      />
      <p className="text-sm leading-relaxed text-muted-foreground">
        Provided by the <code>{server.plugin}</code> plugin. Its configuration
        ships with the package, so it is not edited here — turn the plugin off
        under Plugins to disconnect it and unmount its tools.
      </p>
    </SettingsSection>
  );
}

export function McpPanel({
  client,
  managed = false,
}: {
  client: ApiClient;
  /** On a managed profile the server refuses manual server writes, so the
   * manual half of this panel becomes a read-only view of what is mounted.
   * The gateway endpoints section keeps its toggles: a `gateway_endpoint`
   * definition is exactly the write managed policy admits. */
  managed?: boolean;
}) {
  const [servers, setServers] = useState<McpServerInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [reconnecting, setReconnecting] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Gateway session state, held here because the gateway endpoints section
  // shares this panel's one server list instead of owning a second copy.
  const [signedIn, setSignedIn] = useState(false);
  const [apps, setApps] = useState<GatewayApps | null>(null);
  // Distinguishes "the apps read failed" from "no apps granted": a failure
  // must not make configured mounts masquerade as revoked, nor hide them.
  const [appsFailed, setAppsFailed] = useState(false);
  // Whether any server-list read has succeeded: before one has, a mount
  // toggle would be a write against unknown state, so the rows say so.
  const [serversKnown, setServersKnown] = useState(false);
  const [listError, setListError] = useState<string | null>(null);
  // Bumped by the Retry affordance; re-runs the list effect immediately and
  // restarts its cadence.
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [mounting, setMounting] = useState(false);
  // `dirty`, mirrored for the async work below: a background read or a slow
  // mount write resolves against a render whose captured `dirty` may predate
  // the edit it must not clobber.
  const dirtyRef = useRef(false);
  // Monotonic id for server-list reads: a slow in-flight read must not
  // clobber the fresher list a write (or a newer read) has since installed.
  const requestRef = useRef(0);

  function markDirty(value: boolean) {
    dirtyRef.current = value;
    setDirty(value);
  }

  /** Install a fresh, authoritative server list: wholesale when nothing is
   * unsaved; otherwise reconciled around the draft. Gateway mounts follow
   * the saved configuration — their toggle writes immediately, and the next
   * Save must carry the result instead of reverting it — while manual rows
   * keep the reader's unsaved edits, and edited mount rows refresh only
   * their health. */
  function adoptServers(fresh: McpServerInfo[]) {
    setServers((current) => {
      // Reading the ref inside the updater is sound where a transition
      // detector would not be: it only reads, so a StrictMode double-invoke
      // computes the same list twice.
      if (!dirtyRef.current) return fresh;
      const freshMounts = new Map<string, McpServerInfo>();
      for (const server of fresh) {
        if (server.gateway_endpoint !== null) {
          freshMounts.set(server.gateway_endpoint, server);
        }
      }
      const kept = current.flatMap((server) => {
        if (server.gateway_endpoint === null) return [server];
        const mount = freshMounts.get(server.gateway_endpoint);
        if (mount === undefined) return [];
        freshMounts.delete(server.gateway_endpoint);
        return [
          {
            ...server,
            health: mount.health,
            tool_count: mount.tool_count,
            diagnostic: mount.diagnostic,
            curated: mount.curated,
          },
        ];
      });
      return [...kept, ...freshMounts.values()];
    });
  }

  // An unreachable gateway reads as signed out: the endpoints section then
  // treats entitlements as unknown rather than failing a page whose subject
  // is the local MCP configuration.
  useEffect(() => {
    let cancelled = false;
    client
      .getGatewayStatus()
      .then((status) => {
        if (!cancelled) setSignedIn(status.signed_in);
      })
      .catch(() => {
        if (!cancelled) setSignedIn(false);
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  // Entitled apps are never cached server-side (a revoked grant disappears on
  // the next request), so fetch them fresh whenever the signed-in state turns
  // on. A fetch failure is remembered, so mount rows can say entitlements are
  // unknown instead of claiming anything.
  useEffect(() => {
    if (!signedIn) {
      setApps(null);
      setAppsFailed(false);
      return;
    }
    let cancelled = false;
    client
      .getGatewayApps()
      .then((next) => {
        if (!cancelled) {
          setApps(next);
          setAppsFailed(false);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setApps(null);
          setAppsFailed(true);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [client, signedIn]);

  // The one reader of the server list: the initial load, the Retry
  // affordance, and — while a gateway session exists — a steady cadence, so
  // a mount that degrades after the first read doesn't keep a stale healthy
  // line. A failed read keeps the last-known rows and surfaces a retryable
  // error instead of silently disabling every toggle.
  useEffect(() => {
    const read = async () => {
      const request = ++requestRef.current;
      try {
        const result = await client.listMcpServers();
        if (request !== requestRef.current) return;
        adoptServers(result.servers);
        setServersKnown(true);
        setListError(null);
        setLoading(false);
      } catch (err) {
        if (request !== requestRef.current) return;
        setListError(errorMessage(err));
        setLoading(false);
      }
    };
    void read();
    const timer = signedIn
      ? window.setInterval(() => void read(), MOUNT_REFRESH_MS)
      : null;
    return () => {
      // Invalidate any in-flight read; a re-run issues fresh ids above this.
      requestRef.current += 1;
      if (timer !== null) window.clearInterval(timer);
    };
    // adoptServers touches only refs and state setters, so the effect only
    // re-runs when a read would actually change: client, session, retry.
  }, [client, signedIn, refreshNonce]);

  function update(index: number, change: Partial<McpServerInfo>) {
    markDirty(true);
    setServers((current) =>
      current.map((server, itemIndex) =>
        itemIndex === index ? { ...server, ...change } : server,
      ),
    );
  }

  async function save() {
    setSaving(true);
    setError(null);
    try {
      const result = await client.putMcpServers(
        servers.filter((server) => server.plugin === null).map(definition),
      );
      // Supersede any in-flight background read; this list is fresher.
      requestRef.current += 1;
      setServers(result.servers);
      setServersKnown(true);
      setListError(null);
      markDirty(false);
      toast.success("Saved MCP servers");
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  }

  /** Mount or unmount one endpoint: an immediate, complete configuration
   * write, rebuilt from the live configuration rather than the draft above
   * so it never persists an unsaved edit — nor drops a server saved from
   * elsewhere in the meantime. */
  async function setMounted(slug: string, mounted: boolean) {
    setMounting(true);
    setError(null);
    try {
      const current = (await client.listMcpServers()).servers
        .filter((server) => server.plugin === null)
        .map(definition);
      const without = current.filter(
        (server) => server.gateway_endpoint !== slug,
      );
      const taken = new Set(without.map((server) => server.name));
      const next = mounted
        ? [...without, mountDefinition(slug, mountName(slug, taken))]
        : without;
      const result = await client.putMcpServers(next);
      // Supersede any in-flight background read; this list is fresher.
      requestRef.current += 1;
      adoptServers(result.servers);
      setServersKnown(true);
      setListError(null);
      toast.success(mounted ? `Mounted ${slug}` : `Unmounted ${slug}`);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setMounting(false);
    }
  }

  async function reconnect(name: string) {
    setReconnecting(name);
    setError(null);
    setServers((current) =>
      current.map((server) =>
        server.name === name
          ? { ...server, health: "reconnecting", diagnostic: null }
          : server,
      ),
    );
    try {
      const result = await client.reconnectMcpServer(name);
      requestRef.current += 1;
      setServers(result.servers);
    } catch (err) {
      setError(errorMessage(err));
      try {
        const result = await client.listMcpServers();
        requestRef.current += 1;
        setServers(result.servers);
      } catch {
        // Preserve the reconnect error; reopening Settings performs a full load.
      }
    } finally {
      setReconnecting(null);
    }
  }

  const working = saving || reconnecting !== null || mounting;

  const entitledSlugs = new Set(
    apps?.apps.flatMap((app) => app.mcp_endpoint_slugs) ?? [],
  );
  // Rows are the union of what's entitled and what's configured: a mount
  // whose grant was revoked — or whose session signed out — must keep its
  // row rather than dropping to a bare failing server in the list.
  const endpointSlugs = [
    ...new Set([
      ...entitledSlugs,
      ...servers
        .map((server) => server.gateway_endpoint)
        .filter((slug): slug is string => slug !== null),
    ]),
  ];
  // Signed out, the section still lists configured mounts (toggles off,
  // pointing at sign-in); it disappears only when there is nothing to show —
  // an unpaired profile with no gateway mounts.
  const endpointsVisible =
    endpointSlugs.length > 0 || (signedIn && listError !== null);
  const endpointsSection = endpointsVisible && (
    <GatewayEndpoints
      signedIn={signedIn}
      slugs={endpointSlugs}
      servers={servers}
      serversKnown={serversKnown}
      entitledSlugs={apps?.supported === true ? entitledSlugs : null}
      appsFailed={appsFailed}
      listError={listError}
      working={working}
      onRetry={() => {
        // One error surface: a retry that recovers the list must not leave a
        // stale action error standing beside fresh rows.
        setError(null);
        setRefreshNonce((nonce) => nonce + 1);
      }}
      onToggle={(slug, mounted) => void setMounted(slug, mounted)}
    />
  );
  // A failed list read still surfaces (without the section's Retry) when the
  // section that normally carries it has nothing else to show.
  const fallbackListError = !endpointsVisible && listError !== null && (
    <SettingsError>
      Couldn't read the MCP server list: {listError}
    </SettingsError>
  );

  if (managed) {
    // The compact transport view behind the Advanced disclosure: one row per
    // gateway endpoint — mount toggle, health chip, the apps it serves, a
    // reconnect action, and the diagnostic inline when unhealthy. No inner
    // headings, no per-endpoint cards, and no tool counts: tools belong to
    // the app entries above. Manual servers a profile carried in from before
    // it was managed are app entries too (never started, with the policy
    // diagnostic), so this view is endpoints only.
    return (
      <div className="flex flex-col gap-3" aria-busy={loading || working}>
        {!signedIn && (
          <p className="text-muted-foreground text-xs">
            Sign in to the Model Gateway to mount or unmount endpoints. The
            configured mounts stay listed meanwhile.
          </p>
        )}
        {appsFailed && (
          <p className="text-muted-foreground text-xs">
            Couldn't read your entitlements from the gateway; these are the
            configured mounts.
          </p>
        )}
        {listError !== null && (
          <div className="flex items-center justify-between gap-4">
            <SettingsError>
              Couldn't read the MCP server list: {listError}
            </SettingsError>
            <Button
              type="button"
              variant="outline"
              disabled={working}
              onClick={() => {
                setError(null);
                setRefreshNonce((nonce) => nonce + 1);
              }}
            >
              <RefreshCw size={14} />
              Retry
            </Button>
          </div>
        )}
        {loading ? (
          <p className="text-sm text-muted-foreground">Loading endpoints…</p>
        ) : endpointSlugs.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No gateway endpoints are granted to your teams.
          </p>
        ) : (
          <ul className="flex flex-col gap-2">
            {endpointSlugs.map((slug) => {
              const mounted = servers.find(
                (server) => server.gateway_endpoint === slug,
              );
              const serves =
                apps?.apps
                  .filter(
                    (app) =>
                      app.enabled && app.mcp_endpoint_slugs.includes(slug),
                  )
                  .map((app) => app.name) ?? [];
              const revoked =
                apps?.supported === true &&
                !entitledSlugs.has(slug) &&
                mounted !== undefined;
              return (
                <li
                  key={slug}
                  className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-md border px-3 py-2 text-sm"
                >
                  <code className="font-medium">{slug}</code>
                  <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    mounted
                    <Switch
                      aria-label={`Mount ${slug}`}
                      checked={mounted !== undefined}
                      disabled={!signedIn || working || !serversKnown}
                      onCheckedChange={(checked) =>
                        void setMounted(slug, checked)
                      }
                    />
                  </span>
                  {mounted && <McpHealthChip health={mounted.health} />}
                  {serves.length > 0 && (
                    <span className="text-xs text-muted-foreground">
                      serves: {serves.join(", ")}
                    </span>
                  )}
                  {mounted &&
                    mounted.health !== "initializing" &&
                    mounted.health !== "disabled" && (
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        disabled={working}
                        onClick={() => void reconnect(mounted.name)}
                      >
                        <RefreshCw size={14} />
                        {reconnecting === mounted.name
                          ? "Reconnecting…"
                          : "Reconnect"}
                      </Button>
                    )}
                  {!serversKnown && (
                    <span className="text-xs text-muted-foreground">
                      Mount state unknown.
                    </span>
                  )}
                  {revoked && (
                    <span className="text-xs text-muted-foreground">
                      No longer granted to your teams. Switch off to unmount
                      it.
                    </span>
                  )}
                  {mounted &&
                    mounted.health !== "healthy" &&
                    mounted.health !== "initializing" &&
                    mounted.health !== "reconnecting" &&
                    mounted.diagnostic !== null && (
                      <span className="text-xs text-destructive">
                        {mounted.diagnostic}
                      </span>
                    )}
                </li>
              );
            })}
          </ul>
        )}
        {error && <SettingsError>{error}</SettingsError>}
      </div>
    );
  }

  return (
    <McpKindSection
      description="Connect local stdio tool servers or remote HTTP endpoints without a shell or a desktop restart."
      busy={loading || working}
    >
      {endpointsSection}
      {loading ? (
        <p className="text-sm text-muted-foreground">
          Loading MCP servers…
        </p>
      ) : (
        <>
          {servers.length === 0 && (
            <SettingsSection>
              <p className="text-sm text-muted-foreground">
                No MCP servers configured. Add one to make its tools available
                to new conversations.
              </p>
            </SettingsSection>
          )}

          {servers.map((server, index) =>
            server.plugin !== null ? (
              <PluginServerSection key={index} server={server} />
            ) : (
            <SettingsSection
              key={index}
              title={server.name || `Server ${index + 1}`}
            >
              <SettingsStatus
                tone={healthTone(server.health)}
                label={healthLabel(server.health)}
                description={
                  server.health === "healthy"
                    ? `${server.tool_count} tool${server.tool_count === 1 ? "" : "s"} available to new turns.`
                    : server.diagnostic ??
                      "Save the configuration to verify this server."
                }
              />

              <McpTierChip curated={server.curated} />

              <div className="flex items-center justify-between gap-4">
                <div className="flex-1">
                  <p className="text-sm font-bold">Enabled</p>
                  <p className="text-xs text-muted-foreground">
                    Off keeps the server configured but out of new turns.
                  </p>
                </div>
                <Switch
                  aria-label="Enabled"
                  checked={server.enabled}
                  disabled={working}
                  onCheckedChange={(checked) =>
                    update(index, { enabled: checked })
                  }
                />
              </div>

              <SettingsField
                label="Namespace"
                hint="ASCII letters, numbers, underscores, and hyphens only."
              >
                <Input
                  value={server.name}
                  disabled={working}
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(event) =>
                    update(index, { name: event.target.value })
                  }
                />
              </SettingsField>

              {transportOf(server) === "gateway" && (
                <p className="text-sm text-muted-foreground">
                  Managed by the Model Gateway (endpoint{" "}
                  <code>{server.gateway_endpoint}</code>). Its URL and
                  short-lived credentials come from the signed-in gateway
                  session; mount or unmount it under Gateway endpoints above.
                </p>
              )}

              {transportOf(server) !== "gateway" && (
              <FieldGroup label="Transport">
                <div className="flex gap-4" role="radiogroup" aria-label="Transport">
                  {(["stdio", "http"] as const).map((transport) => (
                    <label
                      key={transport}
                      className="flex items-center gap-2 text-sm"
                    >
                      <input
                        type="radio"
                        name={`transport-${index}`}
                        checked={transportOf(server) === transport}
                        disabled={working}
                        onChange={() =>
                          update(index, transportFields(transport))
                        }
                      />
                      {transport === "stdio"
                        ? "Local process (stdio)"
                        : "Remote endpoint (HTTP)"}
                    </label>
                  ))}
                </div>
              </FieldGroup>
              )}

              {transportOf(server) === "stdio" && (
                <>
                  <SettingsField
                    label="Executable"
                    hint="An executable path or command name. OpenWave never invokes a shell."
                  >
                    <Input
                      value={server.command ?? ""}
                      disabled={working}
                      autoComplete="off"
                      spellCheck={false}
                      placeholder="/absolute/path/to/server"
                      onChange={(event) =>
                        update(index, { command: event.target.value })
                      }
                    />
                  </SettingsField>

                  <StringListEditor
                    label="Arguments"
                    values={server.args}
                    disabled={working}
                    addLabel="Add argument"
                    onChange={(args) => update(index, { args })}
                  />

                  <SettingsField
                    label="Working directory"
                    hint="Optional. It does not grant the server any OpenWave folder capability."
                  >
                    <Input
                      value={server.cwd ?? ""}
                      disabled={working}
                      autoComplete="off"
                      spellCheck={false}
                      placeholder="Optional"
                      onChange={(event) =>
                        update(index, { cwd: event.target.value || null })
                      }
                    />
                  </SettingsField>
                </>
              )}

              {transportOf(server) === "http" && (
                <>
                  <SettingsField
                    label="Server URL"
                    hint="An http or https MCP endpoint. Credentials never go in the URL."
                  >
                    <Input
                      value={server.url ?? ""}
                      disabled={working}
                      autoComplete="off"
                      spellCheck={false}
                      placeholder="https://gateway.example/mcp/tools"
                      onChange={(event) =>
                        update(index, { url: event.target.value })
                      }
                    />
                  </SettingsField>

                  <SettingsField
                    label="Bearer token variable"
                    hint="Optional. Only the name is saved; the token is read from the host environment when connecting and never displayed."
                  >
                    <Input
                      value={server.bearer_token_env ?? ""}
                      disabled={working}
                      autoComplete="off"
                      spellCheck={false}
                      placeholder="GATEWAY_TOKEN"
                      onChange={(event) =>
                        update(index, {
                          bearer_token_env: event.target.value || null,
                        })
                      }
                    />
                  </SettingsField>
                </>
              )}

              <SettingsField
                label="Request timeout (ms)"
                hint={`A whole number from 1 to ${MAX_TIMEOUT_MS.toLocaleString()}.`}
              >
                <Input
                  type="number"
                  inputMode="numeric"
                  min={1}
                  max={MAX_TIMEOUT_MS}
                  value={server.request_timeout_ms}
                  disabled={working}
                  onChange={(event) =>
                    update(index, {
                      request_timeout_ms: Number(event.target.value),
                    })
                  }
                />
              </SettingsField>

              {transportOf(server) === "stdio" && (
                <>
                  <EnvironmentEditor
                    names={server.env}
                    values={server.env_values ?? {}}
                    disabled={working}
                    onChange={(env, env_values) =>
                      update(index, { env, env_values })
                    }
                  />

                  <StringListEditor
                    label="Forward environment names"
                    hint="Only names are saved or displayed. Their values are resolved in the host process and never returned to the app."
                    values={server.env_from}
                    disabled={working}
                    addLabel="Add variable name"
                    onChange={(env_from) => update(index, { env_from })}
                  />
                </>
              )}

              <div className="flex flex-wrap gap-2">
                {server.enabled &&
                  server.health !== "initializing" &&
                  !dirty && (
                  <Button
                    type="button"
                    variant="outline"
                    disabled={working}
                    onClick={() => void reconnect(server.name)}
                  >
                    <RefreshCw size={14} />
                    {reconnecting === server.name
                      ? "Reconnecting…"
                      : "Reconnect and refresh tools"}
                  </Button>
                )}
                {/* Mounts are owned by the mount write, not the draft: a
                    draft deletion would be undone by the next reconcile,
                    which re-adds every configured mount. Unmounting writes
                    immediately, like the section's toggle. */}
                {transportOf(server) === "gateway" ? (
                  <Button
                    type="button"
                    variant="destructive"
                    disabled={working}
                    onClick={() => {
                      const slug = server.gateway_endpoint;
                      if (slug !== null) void setMounted(slug, false);
                    }}
                  >
                    <Trash2 size={14} />
                    Unmount
                  </Button>
                ) : (
                  <Button
                    type="button"
                    variant="destructive"
                    disabled={working}
                    onClick={() => {
                      markDirty(true);
                      setServers((current) =>
                        current.filter((_, itemIndex) => itemIndex !== index),
                      );
                    }}
                  >
                    <Trash2 size={14} />
                    Remove
                  </Button>
                )}
              </div>
            </SettingsSection>
            ),
          )}

          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              variant="outline"
              disabled={working}
              onClick={() => {
                markDirty(true);
                setServers((current) => [
                  ...current,
                  emptyServer(current.length),
                ]);
              }}
            >
              <Plus size={14} />
              Add server
            </Button>
            <Button type="button" disabled={working} onClick={() => void save()}>
              {saving ? "Verifying…" : "Save and verify"}
            </Button>
          </div>

          {dirty && (
            <p className="text-xs text-muted-foreground">
              Save and verify changes before reconnecting a server.
            </p>
          )}

          <p className="text-sm leading-relaxed text-muted-foreground">
            Child environments start empty. Environment values are held in the
            OS credential store and never come back to this window; do not
            enter secrets in the executable, arguments, working directory, or
            a server URL, which are ordinary settings.
          </p>
        </>
      )}
      {fallbackListError}
      {error && <SettingsError>{error}</SettingsError>}
    </McpKindSection>
  );
}

/**
 * The MCP kind-section frame. This panel renders inside the Connected apps
 * page rather than as a page of its own, so it leads with a section heading
 * — the page title, column, and rhythm belong to the surrounding
 * `SettingsPanel` — while `aria-busy` still scopes this kind's own work.
 */
function McpKindSection({
  description,
  busy,
  children,
}: {
  description: string;
  busy: boolean;
  children: ReactNode;
}) {
  return (
    <section className="flex flex-col gap-10" aria-busy={busy}>
      <div className="flex flex-col gap-1">
        <h2 className="text-lg font-semibold">MCP servers</h2>
        <p className="text-sm text-muted-foreground">{description}</p>
      </div>
      {children}
    </section>
  );
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
      return "Disabled in its server entry.";
    default:
      return "Needs attention. See its server entry.";
  }
}

/** A message that can sit mid-sentence: `String(err)` would keep the error
 * class prefix ("HttpError: ...") in front of it. */
function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** A fresh gateway mount: everything comes from the session except the name,
 * which doubles as the tool namespace. */
function mountDefinition(slug: string, name: string): McpServerDefinition {
  return {
    name,
    command: null,
    args: [],
    env: [],
    env_from: [],
    cwd: null,
    url: null,
    bearer_token_env: null,
    gateway_endpoint: slug,
    request_timeout_ms: DEFAULT_TIMEOUT_MS,
    enabled: true,
    plugin: null,
  };
}

/**
 * The gateway's MCP endpoints, and the toggle that mounts each one.
 *
 * Mounting belongs beside the health of what is mounted, so it lives here
 * rather than in the Model Gateway panel, which keeps the connected apps as
 * an informational list. A `gateway_endpoint` definition is the one write
 * managed policy admits, so these toggles stay live on a managed profile
 * where every manual server on this page is read-only.
 *
 * Purely presentational: the panel owns the single server list this section
 * reads, its refresh cadence, and the mount writes, so there is no second
 * copy of the configuration to fall out of step with the editor beside it.
 */
function GatewayEndpoints({
  signedIn,
  slugs,
  servers,
  serversKnown,
  entitledSlugs,
  appsFailed,
  listError,
  working,
  onRetry,
  onToggle,
}: {
  signedIn: boolean;
  slugs: string[];
  servers: McpServerInfo[];
  /** Whether any server-list read has succeeded yet; before one has, mount
   * state is unknown and the rows say so instead of writing blind. */
  serversKnown: boolean;
  /** null while entitlements are unknown — signed out, an older gateway, or
   * a failed apps read — so no row ever claims a revocation it can't know. */
  entitledSlugs: ReadonlySet<string> | null;
  appsFailed: boolean;
  listError: string | null;
  working: boolean;
  onRetry: () => void;
  onToggle: (slug: string, mounted: boolean) => void;
}) {
  /** The one line under a mount row: unknown beats revoked beats health. */
  const rowNote = (slug: string, mounted: McpServerInfo | undefined) => {
    if (!serversKnown) return "Mount state unknown.";
    if (entitledSlugs !== null && !entitledSlugs.has(slug)) {
      return "No longer granted to your teams. Switch off to unmount it.";
    }
    return mounted ? mountStatus(mounted) : null;
  };

  return (
    <SettingsSection
      title="Gateway endpoints"
      description="Mounted endpoints connect with your gateway session — no tokens to copy, and they reconnect after you sign back in."
    >
      {!signedIn && (
        <p className="text-muted-foreground text-xs">
          Sign in to the Model Gateway to mount or unmount endpoints. The
          configured mounts stay listed meanwhile.
        </p>
      )}
      {appsFailed && (
        <p className="text-muted-foreground text-xs">
          Couldn't read your entitlements from the gateway; these are the
          configured mounts.
        </p>
      )}
      {listError !== null && (
        <div className="flex items-center justify-between gap-4">
          <SettingsError>
            Couldn't read the MCP server list: {listError}
          </SettingsError>
          <Button
            type="button"
            variant="outline"
            disabled={working}
            onClick={onRetry}
          >
            <RefreshCw size={14} />
            Retry
          </Button>
        </div>
      )}
      {slugs.length > 0 && (
        <ul className="flex flex-col gap-2">
          {slugs.map((slug) => {
            const mounted = servers.find(
              (server) => server.gateway_endpoint === slug,
            );
            const note = rowNote(slug, mounted);
            return (
              <li
                key={slug}
                className="flex items-center justify-between gap-4 rounded-md border px-3 py-2 text-sm"
              >
                <div className="min-w-0 flex-1">
                  <code className="font-medium">{slug}</code>
                  {note && (
                    <p className="text-muted-foreground text-xs">{note}</p>
                  )}
                </div>
                <Switch
                  aria-label={`Mount ${slug}`}
                  checked={mounted !== undefined}
                  disabled={!signedIn || working || !serversKnown}
                  onCheckedChange={(checked) => onToggle(slug, checked)}
                />
              </li>
            );
          })}
        </ul>
      )}
    </SettingsSection>
  );
}

function StringListEditor({
  label,
  hint,
  values,
  disabled,
  addLabel,
  onChange,
}: {
  label: string;
  hint?: string;
  values: string[];
  disabled: boolean;
  addLabel: string;
  onChange: (values: string[]) => void;
}) {
  return (
    <FieldGroup label={label} hint={hint}>
      <div className="flex flex-col gap-2">
        {values.map((value, index) => (
          <div className="flex gap-2" key={index}>
            <Input
              aria-label={`${label} ${index + 1}`}
              value={value}
              disabled={disabled}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) =>
                onChange(
                  values.map((item, itemIndex) =>
                    itemIndex === index ? event.target.value : item,
                  ),
                )
              }
            />
            <Button
              type="button"
              variant="outline"
              aria-label={`Remove ${label.toLowerCase()} ${index + 1}`}
              disabled={disabled}
              onClick={() =>
                onChange(values.filter((_, itemIndex) => itemIndex !== index))
              }
            >
              <Trash2 size={14} />
            </Button>
          </div>
        ))}
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={disabled}
          onClick={() => onChange([...values, ""])}
        >
          <Plus size={14} />
          {addLabel}
        </Button>
      </div>
    </FieldGroup>
  );
}

/**
 * The child's own environment: names on the left, values on the right.
 *
 * The server returns names only — a stored value never comes back — so an
 * existing row's value field starts blank and staying blank keeps whatever is
 * stored. Typing replaces it. That is why the inputs are password-style
 * despite the label: people put API keys here regardless of what the label
 * says, so the field is built for the credential case.
 */
function EnvironmentEditor({
  names,
  values,
  disabled,
  onChange,
}: {
  names: string[];
  values: Record<string, string>;
  disabled: boolean;
  onChange: (names: string[], values: Record<string, string>) => void;
}) {
  function replaceRow(index: number, name: string, value: string | null) {
    const previous = names[index];
    const nextNames = names.map((item, itemIndex) =>
      itemIndex === index ? name : item,
    );
    const nextValues: Record<string, string> = {};
    for (const [key, item] of Object.entries(values)) {
      if (key !== previous) nextValues[key] = item;
    }
    // A renamed row cannot keep a value it never showed: the stored value
    // belongs to the old name, and the new one starts unset.
    const carried = value ?? (name === previous ? values[previous] : undefined);
    if (carried !== undefined && carried !== "") nextValues[name] = carried;
    onChange(nextNames, nextValues);
  }
  return (
    <FieldGroup
      label="Environment"
      hint="Values are held in the OS credential store, never in settings, and are never sent back to this window. Leave a value blank to keep the one already stored."
    >
      <div className="flex flex-col gap-2">
        {names.map((name, index) => (
          <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] gap-2" key={index}>
            <Input
              aria-label={`Environment name ${index + 1}`}
              placeholder="NAME"
              value={name}
              disabled={disabled}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => replaceRow(index, event.target.value, null)}
            />
            <Input
              type="password"
              aria-label={`Environment value ${index + 1}`}
              placeholder="leave blank to keep"
              value={values[name] ?? ""}
              disabled={disabled}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) =>
                replaceRow(index, name, event.target.value)
              }
            />
            <Button
              type="button"
              variant="outline"
              aria-label={`Remove environment value ${index + 1}`}
              disabled={disabled}
              onClick={() => {
                const nextValues = { ...values };
                delete nextValues[name];
                onChange(
                  names.filter((_, itemIndex) => itemIndex !== index),
                  nextValues,
                );
              }}
            >
              <Trash2 size={14} />
            </Button>
          </div>
        ))}
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={disabled}
          onClick={() => onChange([...names, ""], values)}
        >
          <Plus size={14} />
          Add variable
        </Button>
      </div>
    </FieldGroup>
  );
}

function FieldGroup({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <span className="text-sm font-medium">{label}</span>
      {children}
      {hint && <span className="text-xs text-muted-foreground">{hint}</span>}
    </div>
  );
}

function healthLabel(health: McpServerInfo["health"]): string {
  switch (health) {
    case "healthy":
      return "Healthy";
    case "degraded":
      return "Needs attention";
    case "reconnecting":
      return "Reconnecting";
    case "disabled":
      return "Disabled";
    case "initializing":
      return "Not verified";
  }
}

function healthTone(
  health: McpServerInfo["health"],
): "ready" | "not-configured" | "disabled" {
  if (health === "healthy") return "ready";
  if (health === "disabled") return "disabled";
  return "not-configured";
}
