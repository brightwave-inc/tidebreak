import { useEffect, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Plus, RefreshCw, Trash2 } from "lucide-react";
import type {
  ApiClient,
  GatewayApps,
  McpServerDefinition,
  McpServerInfo,
} from "../api";
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
    env: {},
    env_from: [],
    cwd: null,
    url: null,
    bearer_token_env: null,
    gateway_endpoint: null,
    request_timeout_ms: DEFAULT_TIMEOUT_MS,
    enabled: true,
    health: "initializing",
    tool_count: 0,
    diagnostic: null,
  };
}

type Transport = "stdio" | "http" | "gateway";

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
        env: {},
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
  const { health: _, tool_count: __, diagnostic: ___, ...value } = server;
  return value;
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

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void client
      .listMcpServers()
      .then((result) => {
        if (!cancelled) {
          setServers(result.servers);
          setDirty(false);
        }
      })
      .catch((err) => {
        if (!cancelled) setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  function update(index: number, change: Partial<McpServerInfo>) {
    setDirty(true);
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
      const result = await client.putMcpServers(servers.map(definition));
      setServers(result.servers);
      setDirty(false);
      toast.success("Saved MCP servers");
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  /** A mount toggled in the gateway section rewrites the same configuration
   * this list edits, so adopt its result — unless the reader has unsaved
   * edits here, which must never be replaced underneath them. */
  function adoptMounts(next: McpServerInfo[]) {
    if (!dirty) setServers(next);
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
      setServers(result.servers);
    } catch (err) {
      setError(String(err));
      try {
        const result = await client.listMcpServers();
        setServers(result.servers);
      } catch {
        // Preserve the reconnect error; reopening Settings performs a full load.
      }
    } finally {
      setReconnecting(null);
    }
  }

  const working = saving || reconnecting !== null;

  if (managed) {
    return (
      <SettingsPanel
        title="MCP servers"
        description="Tool servers provided by your organization's model gateway."
        busy={loading}
      >
        <GatewayEndpoints client={client} onMountsChanged={adoptMounts} />
        {loading ? (
          <p className="text-sm text-muted-foreground">Loading MCP servers…</p>
        ) : (
          <ManagedServerList servers={servers} />
        )}
        {error && <SettingsError>{error}</SettingsError>}
      </SettingsPanel>
    );
  }

  return (
    <SettingsPanel
      title="MCP servers"
      description="Connect local stdio tool servers or remote HTTP endpoints without a shell or a desktop restart."
      busy={loading || working}
    >
      <GatewayEndpoints client={client} onMountsChanged={adoptMounts} />
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

          {servers.map((server, index) => (
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
                    values={server.env}
                    disabled={working}
                    onChange={(env) => update(index, { env })}
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
                <Button
                  type="button"
                  variant="destructive"
                  disabled={working}
                  onClick={() =>
                    setServers((current) => {
                      setDirty(true);
                      return current.filter(
                        (_, itemIndex) => itemIndex !== index,
                      );
                    })
                  }
                >
                  <Trash2 size={14} />
                  Remove
                </Button>
              </div>
            </SettingsSection>
          ))}

          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              variant="outline"
              disabled={working}
              onClick={() =>
                setServers((current) => {
                  setDirty(true);
                  return [...current, emptyServer(current.length)];
                })
              }
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
            MCP tools are always sensitive and keep OpenWave’s existing approval
            boundary. Child environments start empty. Put credentials in the
            host environment and select only their variable names above; do not
            enter secrets in the executable, arguments, working directory,
            literal values, or a server URL.
          </p>
        </>
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
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
      return "Disabled below.";
    default:
      return "Needs attention. See its entry below.";
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
    env: {},
    env_from: [],
    cwd: null,
    url: null,
    bearer_token_env: null,
    gateway_endpoint: slug,
    request_timeout_ms: DEFAULT_TIMEOUT_MS,
    enabled: true,
  };
}

/**
 * The gateway's MCP endpoints, and the toggle that mounts each one.
 *
 * Mounting belongs beside the health of what is mounted, so it lives here
 * rather than in the Model Gateway panel, which keeps the connected apps as
 * an informational list. A `gateway_endpoint` definition is the one write
 * managed policy admits, so these toggles stay live on a managed profile
 * where every manual server below is read-only.
 *
 * The section carries its own view of the configured servers, separate from
 * the editable list around it, so a background refresh can keep the health
 * lines honest without touching a form the reader is part-way through.
 * Nothing renders at all until a gateway session exists.
 */
function GatewayEndpoints({
  client,
  onMountsChanged,
}: {
  client: ApiClient;
  /** The fresh server list a mount write returns, so the page around this
   * section reflects the mount it just made. */
  onMountsChanged: (servers: McpServerInfo[]) => void;
}) {
  const [signedIn, setSignedIn] = useState(false);
  const [apps, setApps] = useState<GatewayApps | null>(null);
  // Distinguishes "the apps read failed" from "no apps granted": a failure
  // must not make configured mounts masquerade as revoked, nor hide them.
  const [appsFailed, setAppsFailed] = useState(false);
  const [servers, setServers] = useState<McpServerInfo[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  // Bumped by the Retry affordance; re-runs the mount-list effect immediately
  // and restarts its cadence.
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Monotonic id for mount-list reads: a slow in-flight read must not clobber
  // the fresher list a toggle write (or a newer read) has since installed.
  const requestRef = useRef(0);

  // An unreachable or unpaired gateway is simply no section, not an error on
  // a page whose subject is the local MCP configuration.
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
  // on. A fetch failure hides the apps list but is remembered, so mount rows
  // can say entitlements are unknown instead of claiming anything.
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

  // Keep re-reading the configuration while the section is visible, so a
  // mount that degrades after the first read doesn't keep a stale healthy
  // line. A failed read keeps the last-known rows and surfaces a retryable
  // error instead of silently disabling every toggle.
  useEffect(() => {
    if (!signedIn) {
      setServers(null);
      setListError(null);
      return;
    }
    const refresh = async () => {
      const request = ++requestRef.current;
      try {
        const next = await client.listMcpServers();
        if (request !== requestRef.current) return;
        setServers(next.servers);
        setListError(null);
      } catch (err) {
        if (request !== requestRef.current) return;
        setListError(errorMessage(err));
      }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), MOUNT_REFRESH_MS);
    return () => {
      // Invalidate any in-flight read; a re-run issues fresh ids above this.
      requestRef.current += 1;
      window.clearInterval(timer);
    };
  }, [client, signedIn, refreshNonce]);

  async function setMounted(slug: string, mounted: boolean) {
    setWorking(true);
    setError(null);
    try {
      // Rebuild from the live configuration, not this section's cache, so a
      // mount toggled here never drops a server edited elsewhere meanwhile.
      const current = (await client.listMcpServers()).servers.map(definition);
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
      setServers(result.servers);
      setListError(null);
      onMountsChanged(result.servers);
      toast.success(mounted ? `Mounted ${slug}` : `Unmounted ${slug}`);
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  const entitledSlugs = new Set(
    apps?.apps.flatMap((app) => app.mcp_endpoint_slugs) ?? [],
  );
  // Rows are the union of what's entitled and what's configured: a mount
  // whose grant was revoked must keep its row (and unmount toggle), not drop
  // to being visible only as a failing server further down the page.
  const endpointSlugs = [
    ...new Set([
      ...entitledSlugs,
      ...(servers ?? [])
        .map((server) => server.gateway_endpoint)
        .filter((slug): slug is string => slug !== null),
    ]),
  ];
  // "Revoked" is only a claim we can make after actually reading the
  // entitlements; a failed or unsupported apps read leaves them unknown.
  const entitlementsKnown = apps?.supported === true;
  /** The one line under a mount row: unknown beats revoked beats health. */
  const rowNote = (slug: string, mounted: McpServerInfo | undefined) => {
    if (servers === null) return "Mount state unknown.";
    if (entitlementsKnown && !entitledSlugs.has(slug)) {
      return "No longer granted to your teams. Switch off to unmount it.";
    }
    return mounted ? mountStatus(mounted) : null;
  };

  // Deliberately NOT gated on the apps read: a configured mount keeps its row
  // — and a list failure its Retry — even when entitlements can't be read.
  if (!signedIn || (endpointSlugs.length === 0 && listError === null)) {
    return null;
  }

  return (
    <SettingsSection
      title="Gateway endpoints"
      description="Mounted endpoints connect with your gateway session — no tokens to copy, and they reconnect after you sign back in."
    >
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
            onClick={() => setRefreshNonce((nonce) => nonce + 1)}
          >
            <RefreshCw size={14} />
            Retry
          </Button>
        </div>
      )}
      {endpointSlugs.length > 0 && (
        <ul className="flex flex-col gap-2">
          {endpointSlugs.map((slug) => {
            const mounted = servers?.find(
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
                  disabled={working || servers === null}
                  onCheckedChange={(checked) => void setMounted(slug, checked)}
                />
              </li>
            );
          })}
        </ul>
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsSection>
  );
}

/**
 * The managed read-only view: what is mounted and whether it is working.
 *
 * Manual servers a profile carried in from before it was managed stay listed
 * with the server's own diagnostic explaining that policy turned them off —
 * a row that vanished would look like data loss, and one that looked editable
 * would be a write the server refuses.
 */
function ManagedServerList({ servers }: { servers: McpServerInfo[] }) {
  return (
    <>
      {servers.length === 0 && (
        <SettingsSection>
          <p className="text-sm text-muted-foreground">
            No MCP servers are mounted. Mount the endpoints you are entitled to
            under Gateway endpoints above.
          </p>
        </SettingsSection>
      )}
      {/* Keyed by name, unlike the editable list: nothing renames a server
          here, and the identity is what the reader sees. */}
      {servers.map((server, index) => (
        <SettingsSection
          key={server.name || index}
          title={server.name || `Server ${index + 1}`}
        >
          <SettingsStatus
            tone={healthTone(server.health)}
            label={healthLabel(server.health)}
            description={
              server.health === "healthy"
                ? `${server.tool_count} tool${server.tool_count === 1 ? "" : "s"} available to new turns.`
                : server.diagnostic ?? "This server is not connected."
            }
          />
          {transportOf(server) === "gateway" ? (
            <p className="text-sm text-muted-foreground">
              Managed by the Model Gateway (endpoint{" "}
              <code>{server.gateway_endpoint}</code>). Mount or unmount it under
              Gateway endpoints above.
            </p>
          ) : (
            <p className="text-sm text-muted-foreground">
              Configured before this device was managed. It is kept on file but
              never started, and cannot be edited here.
            </p>
          )}
        </SettingsSection>
      ))}
      <p className="text-sm leading-relaxed text-muted-foreground">
        MCP tools are always sensitive and keep OpenWave&rsquo;s existing
        approval boundary.
      </p>
    </>
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

function EnvironmentEditor({
  values,
  disabled,
  onChange,
}: {
  values: Record<string, string>;
  disabled: boolean;
  onChange: (values: Record<string, string>) => void;
}) {
  const rows = Object.entries(values);
  function replaceRow(index: number, name: string, value: string) {
    const next = rows.map(([key, item], itemIndex) =>
      itemIndex === index ? ([name, value] as const) : ([key, item] as const),
    );
    onChange(Object.fromEntries(next));
  }
  return (
    <FieldGroup
      label="Literal non-secret environment"
      hint="These values are displayed and stored as ordinary settings. Never put credentials here."
    >
      <div className="flex flex-col gap-2">
        {rows.map(([name, value], index) => (
          <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] gap-2" key={index}>
            <Input
              aria-label={`Environment name ${index + 1}`}
              placeholder="NAME"
              value={name}
              disabled={disabled}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) =>
                replaceRow(index, event.target.value, value)
              }
            />
            <Input
              aria-label={`Environment value ${index + 1}`}
              placeholder="non-secret value"
              value={value}
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
              onClick={() =>
                onChange(
                  Object.fromEntries(
                    rows.filter((_, itemIndex) => itemIndex !== index),
                  ),
                )
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
          onClick={() => onChange({ ...values, "": "" })}
        >
          <Plus size={14} />
          Add literal value
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
