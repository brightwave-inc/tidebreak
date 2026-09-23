import { useEffect, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { ExternalLink, Plus, RefreshCw, Trash2, Upload } from "lucide-react";
import {
  HttpError,
  type ApiClient,
  type GatewayApps,
  type McpDirectoryEntry,
  type McpHealth,
  type McpOAuthStatus,
  type McpServerDefinition,
  type McpServerInfo,
} from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Switch } from "@/components/ui/switch";
import { Spinner } from "@/components/ui/spinner";
import { hostMachineLabel } from "@/remoteMachine";
import { attachedRemotely } from "@/host";
import { openInBrowser } from "@/openInBrowser";
import { McpDirectoryList } from "./McpDirectory";
import { McpTierChip } from "./McpTierChip";
import {
  SettingsError,
  SettingsField,
  SettingsSection,
  SettingsStatus,
  type SettingsStatusTone,
} from "./primitives";
import {
  parseMcpImportText,
  type McpImportResult,
  type McpImportSecret,
} from "./mcpImport";

const DEFAULT_TIMEOUT_MS = 60_000;
const MAX_TIMEOUT_MS = 3_600_000;
const MAX_IMPORT_BYTES = 1024 * 1024;
/** Mount health lives in the local MCP supervisor, so a modest refresh while
 * the section is visible keeps the health lines honest without gateway load. */
const MOUNT_REFRESH_MS = 15_000;
/** Server names cap at 32 bytes (the MCP tool namespace); endpoint slugs go
 * to 127, so the mount name is derived, not the slug itself. Mount identity
 * is always the `gateway_endpoint` field, never the name. */
const MAX_NAMESPACE_BYTES = 32;
/** How often the list is read while a sign-in waits on the browser, or while
 * a saved server is still connecting. The server gives up on a sign-in after
 * five minutes and bounds every connection attempt, so this ends too. */
const SIGN_IN_POLL_MS = 2_000;

type McpImportSummary = McpImportResult & { fileName: string };

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
    oauth: false,
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
      ? "text-success"
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
 * The sign-in action for one HTTP MCP server that uses OAuth: Connect,
 * reopen or cancel the page while a sign-in waits, or Disconnect. What the
 * state means is said once, by the status line above it
 * ({@link mcpServerStatus}), so this carries actions, and the host the
 * sign-in page is on, so the person sees where Connect sends them.
 */
export function McpOAuthControl({
  status,
  busy = false,
  disabled = false,
  onConnect,
  onDisconnect,
  onCancel,
}: {
  status: McpOAuthStatus;
  busy?: boolean;
  disabled?: boolean;
  onConnect?: () => void;
  onDisconnect?: () => void;
  onCancel?: () => void;
}) {
  const host = status.sign_in_host;
  const connect = (label: string) => (
    <div className="flex flex-wrap items-center gap-2">
      <Button
        type="button"
        size="sm"
        disabled={disabled || busy}
        onClick={onConnect}
      >
        {busy && <Spinner aria-hidden className="size-3.5" />}
        {busy ? "Connecting…" : label}
      </Button>
      {host ? (
        <span className="text-xs text-muted-foreground">Opens {host}</span>
      ) : null}
    </div>
  );
  switch (status.state) {
    case "unsupported":
      return null;
    case "not_connected":
      return connect("Connect");
    case "expired":
      return connect("Reconnect");
    case "access_denied":
      return connect("Try again");
    case "authorizing":
      return (
        <div className="flex flex-wrap items-center gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={disabled || !status.pending_authorization_url}
            onClick={() => {
              const url = status.pending_authorization_url;
              if (url) void openInBrowser(url);
            }}
          >
            <ExternalLink />
            Reopen sign-in page
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={disabled || busy}
            onClick={onCancel}
          >
            Cancel
          </Button>
        </div>
      );
    case "connected":
      return (
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-xs text-muted-foreground">
            {host ? `Signed in with ${host}` : "Signed in"}
          </span>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={disabled || busy}
            onClick={onDisconnect}
          >
            {busy ? "Disconnecting…" : "Disconnect"}
          </Button>
        </div>
      );
  }
}

/** The status line for a server: its tone, a short verdict, and one
 * sentence on what to do next. */
export type McpServerStatus = {
  tone: SettingsStatusTone;
  label: string;
  description: string;
};

/** What the status line says in a window attached to another machine: the
 * sign-in page returns to the machine that runs the server, not to this
 * computer, so a sign-in started here cannot finish here. */
const REMOTE_SIGN_IN =
  "This window is attached to another machine. The sign-in page returns to the machine that runs this server, so the sign-in has to finish in a browser on that machine.";

/**
 * What a configured server's status line says. A saved server still making
 * its first connection, as every saved server is right after Tidebreak
 * starts, says it is connecting. A server that is not connected because it
 * waits on an OAuth sign-in says so, and why, instead of reading as a failed
 * connection; one that just signed in says it is connecting; any other server
 * reads from its health. Attached to another machine (`remote`), the sign-in
 * states say plainly that the sign-in has to finish on that machine. `saved`
 * tells a saved server from an unsaved row, which also reads `initializing`.
 */
export function mcpServerStatus(
  server: McpServerInfo,
  { remote = false, saved = false }: { remote?: boolean; saved?: boolean } = {},
): McpServerStatus {
  if (saved && server.health === "initializing") {
    return {
      tone: "neutral",
      label: "Connecting",
      description:
        "Tidebreak is connecting to this server. Its tools reach new turns once it is up.",
    };
  }
  const oauth = oauthStatusOf(server);
  if (oauth?.state === "connected" && server.health === "reconnecting") {
    return {
      tone: "neutral",
      label: "Connecting",
      description:
        "You are signed in. Tidebreak is connecting to the server and loading its tools.",
    };
  }
  const unconnected =
    server.health === "degraded" || server.health === "initializing";
  if (oauth !== null && unconnected) {
    const host = oauth.sign_in_host;
    switch (oauth.state) {
      case "not_connected":
        return {
          tone: "warning",
          label: "Sign in required",
          description: remote
            ? REMOTE_SIGN_IN
            : (oauth.error ??
              server.diagnostic ??
              "Select Connect to sign in with your browser."),
        };
      case "authorizing":
        return {
          tone: "neutral",
          label: "Waiting for sign-in",
          description: remote
            ? REMOTE_SIGN_IN
            : `Finish signing in on ${host ?? "the page Tidebreak opened"} in your browser. This page updates on its own.`,
        };
      case "expired":
        return {
          tone: "warning",
          label: "Sign-in expired",
          description: remote
            ? REMOTE_SIGN_IN
            : (oauth.error ?? "Select Reconnect to sign in again."),
        };
      case "access_denied":
        return {
          tone: "warning",
          label: "Sign-in denied",
          description: remote
            ? REMOTE_SIGN_IN
            : (oauth.error ?? "Select Try again to start over."),
        };
      case "unsupported":
        return {
          tone: "critical",
          label: "Sign-in not supported",
          description:
            server.diagnostic ??
            oauth.error ??
            "Tidebreak cannot complete this server's sign-in.",
        };
      case "connected":
        break;
    }
  }
  return {
    tone: healthTone(server.health),
    label: healthLabel(server.health),
    description: verifyDescription(server),
  };
}

/**
 * The top of a configured server's settings: the status line, then one row
 * with the server's tier and, for a server that signs in, its sign-in
 * action. The row keeps the chip and the button at their own size instead of
 * stretching them across the column.
 */
export function McpServerSummary({
  server,
  busy = false,
  disabled = false,
  remote = false,
  saved = false,
  onConnect,
  onDisconnect,
  onCancel,
}: {
  server: McpServerInfo;
  busy?: boolean;
  disabled?: boolean;
  /** Whether this window is attached to another machine. */
  remote?: boolean;
  /** Whether the row is a saved server rather than an unsaved edit. */
  saved?: boolean;
  onConnect?: () => void;
  onDisconnect?: () => void;
  onCancel?: () => void;
}) {
  const oauth = oauthStatusOf(server);
  return (
    <>
      <SettingsStatus {...mcpServerStatus(server, { remote, saved })} />
      <div className="flex flex-wrap items-center gap-2">
        <McpTierChip curated={server.curated} />
        {oauth ? (
          <McpOAuthControl
            status={oauth}
            busy={busy}
            disabled={disabled}
            onConnect={onConnect}
            onDisconnect={onDisconnect}
            onCancel={onCancel}
          />
        ) : null}
      </div>
    </>
  );
}

/** Whether a server is still on its way to connected: reconnecting, or not
 * verified yet. */
function settling(health: McpHealth): boolean {
  return health === "reconnecting" || health === "initializing";
}

/** The OAuth status the server reported for an HTTP server, or `null` when
 * nothing about it involves OAuth. The server decides: a saved `oauth` flag
 * is not needed, so an imported server that asks for a sign-in gets one. */
function oauthStatusOf(server: McpServerInfo): McpOAuthStatus | null {
  if (transportOf(server) !== "http") return null;
  return server.oauth_status ?? null;
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
        oauth: false,
      }
    : {
        command: "",
        url: null,
        bearer_token_env: null,
        gateway_endpoint: null,
        oauth: false,
      };
}

/** The definition half of a listed server: every projection field comes
 * off, because the server refuses a definition with a field it does not
 * know. */
function definition(server: McpServerInfo): McpServerDefinition {
  const {
    health: _,
    tool_count: __,
    diagnostic: ___,
    curated: ____,
    resolved_command: _____,
    oauth_status: ______,
    ...value
  } = server;
  return value;
}

/** A draft row with the server's latest projection of the saved server of
 * the same name: health, tools, diagnostic, and sign-in state. The row's own
 * edits stay. */
function withProjection(
  server: McpServerInfo,
  saved: McpServerInfo,
): McpServerInfo {
  const { oauth_status: _, ...draft } = server;
  return {
    ...draft,
    health: saved.health,
    tool_count: saved.tool_count,
    diagnostic: saved.diagnostic,
    curated: saved.curated,
    ...(saved.oauth_status ? { oauth_status: saved.oauth_status } : {}),
  };
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
        description={verifyDescription(server)}
      />
      <p className="text-sm leading-relaxed text-muted-foreground">
        Provided by the <code>{server.plugin}</code> plugin. Its configuration
        ships with the package, so it is not edited here — turn the plugin off
        under Plugins to disconnect it and turn off its tools.
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
  const [importing, setImporting] = useState(false);
  const [reconnecting, setReconnecting] = useState<string | null>(null);
  const [oauthWorking, setOauthWorking] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [importError, setImportError] = useState<string | null>(null);
  const [importSummary, setImportSummary] = useState<McpImportSummary | null>(
    null,
  );
  const [importDraft, setImportDraft] = useState("");
  const importInputRef = useRef<HTMLInputElement>(null);
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
  // The servers whose sign-in was waiting at the last read, so the read that
  // sees one connect can say so.
  const signingInRef = useRef(new Set<string>());
  // The names the last authoritative list held. A row with one of them is a
  // saved server, so `initializing` there means connecting, not unsaved.
  const [savedNames, setSavedNames] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  // The directory of remote servers: `null` while it loads.
  const [directory, setDirectory] = useState<McpDirectoryEntry[] | null>(null);
  const [directoryError, setDirectoryError] = useState<string | null>(null);
  // The directory entry being added, and why the last add failed.
  const [adding, setAdding] = useState<string | null>(null);
  const [addError, setAddError] = useState<string | null>(null);

  function markDirty(value: boolean) {
    dirtyRef.current = value;
    setDirty(value);
  }

  /** Note which servers an authoritative list says are saved. */
  function rememberSaved(fresh: McpServerInfo[]) {
    setSavedNames(new Set(fresh.map((server) => server.name)));
  }

  /** Install a fresh, authoritative server list: wholesale when nothing is
   * unsaved; otherwise reconciled around the draft. Gateway mounts follow
   * the saved configuration — their toggle writes immediately, and the next
   * Save must carry the result instead of reverting it — while manual rows
   * keep the reader's unsaved edits and refresh only their projection, so a
   * sign-in that finishes mid-edit still shows. */
  function adoptServers(fresh: McpServerInfo[]) {
    rememberSaved(fresh);
    setServers((current) => {
      // Reading the ref inside the updater is sound where a transition
      // detector would not be: it only reads, so a StrictMode double-invoke
      // computes the same list twice.
      if (!dirtyRef.current) return fresh;
      const freshMounts = new Map<string, McpServerInfo>();
      const freshManual = new Map<string, McpServerInfo>();
      for (const server of fresh) {
        if (server.gateway_endpoint !== null) {
          freshMounts.set(server.gateway_endpoint, server);
        } else if (server.plugin === null) {
          freshManual.set(server.name, server);
        }
      }
      const kept = current.flatMap((server) => {
        if (server.gateway_endpoint === null) {
          const saved = freshManual.get(server.name);
          return [
            saved && server.plugin === null
              ? withProjection(server, saved)
              : server,
          ];
        }
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

  // The directory is static data the server compiles in, so one read per
  // visit is enough. A managed profile locks remote servers a person adds,
  // so it gets no directory.
  useEffect(() => {
    if (managed) return;
    let cancelled = false;
    client
      .getMcpDirectory()
      .then((result) => {
        if (!cancelled) setDirectory(result.servers);
      })
      .catch((err: unknown) => {
        if (!cancelled) setDirectoryError(errorMessage(err));
      });
    return () => {
      cancelled = true;
    };
  }, [client, managed]);

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
      rememberSaved(result.servers);
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

  function applyImport(text: string, source: string) {
    const result = parseMcpImportText(text, servers);
    setImportSummary({ ...result, fileName: source });
    if (result.servers.length > 0) {
      markDirty(true);
      setServers((current) => [...current, ...result.servers]);
    }
  }

  async function importConfiguration(file: File) {
    setImporting(true);
    setImportError(null);
    setImportSummary(null);
    try {
      if (file.size > MAX_IMPORT_BYTES) {
        throw new Error("Choose a JSON file no larger than 1 MB.");
      }
      const text = await file.text();
      setImportDraft(text);
      applyImport(text, file.name);
    } catch (err) {
      setImportError(`Could not import ${file.name}: ${errorMessage(err)}`);
    } finally {
      setImporting(false);
    }
  }

  function importPastedJson() {
    setImporting(true);
    setImportError(null);
    setImportSummary(null);
    try {
      applyImport(importDraft, "pasted JSON");
    } catch (err) {
      setImportError(`Could not import pasted JSON: ${errorMessage(err)}`);
    } finally {
      setImporting(false);
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
      toast.success(mounted ? `Connected ${slug}` : `Disconnected ${slug}`);
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
      rememberSaved(result.servers);
    } catch (err) {
      setError(errorMessage(err));
      try {
        const result = await client.listMcpServers();
        requestRef.current += 1;
        setServers(result.servers);
        rememberSaved(result.servers);
      } catch {
        // Preserve the reconnect error; reopening Settings performs a full load.
      }
    } finally {
      setReconnecting(null);
    }
  }

  /** One authoritative read of the server list, superseding any older read
   * still in flight. A failed read keeps the rows on screen. */
  async function refreshServers() {
    const request = ++requestRef.current;
    try {
      const result = await client.listMcpServers();
      if (request !== requestRef.current) return;
      announceFinishedSignIns(result.servers);
      adoptServers(result.servers);
      setServersKnown(true);
      setListError(null);
    } catch {
      // Keep the last rows; the next read or a reopened Settings recovers.
    }
  }

  /** Say so when a sign-in this panel was waiting on lands. A sign-in is
   * followed while it waits on the browser and while the server reconnects
   * after it, and ends when the server is healthy (a toast) or the sign-in
   * stops (its row says why, so no toast). */
  function announceFinishedSignIns(fresh: McpServerInfo[]) {
    const following = new Set<string>();
    for (const server of fresh) {
      const state = server.oauth_status?.state;
      if (state === "authorizing") {
        following.add(server.name);
      } else if (signingInRef.current.has(server.name)) {
        if (state === "connected" && server.health === "healthy") {
          toast.success(`Connected ${server.name}`);
        } else if (state === "connected" && settling(server.health)) {
          following.add(server.name);
        }
      }
    }
    signingInRef.current = following;
  }

  /** Show one server's new sign-in state before the next read lands. */
  function showOauthStatus(name: string, status: McpOAuthStatus) {
    setServers((current) =>
      current.map((server) =>
        server.name === name ? { ...server, oauth_status: status } : server,
      ),
    );
  }

  /** Start a sign-in and open its page in the person's browser, from this
   * computer: the server that runs the sign-in may be another machine. The
   * list read that follows shows the wait, and the reads while it lasts show
   * how it ended. */
  async function connectOauth(name: string) {
    setOauthWorking(name);
    setError(null);
    try {
      const status = await client.connectMcpServer(name);
      showOauthStatus(name, status);
      const page = status.pending_authorization_url;
      if (status.state === "authorizing" && page) {
        signingInRef.current.add(name);
        await openInBrowser(page);
        toast.message(`Finish signing in to ${name} in your browser`);
      }
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setOauthWorking(null);
    }
    await refreshServers();
  }

  async function cancelOauth(name: string) {
    setOauthWorking(name);
    setError(null);
    try {
      await client.cancelMcpServerConnect(name);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setOauthWorking(null);
    }
    await refreshServers();
  }

  async function disconnectOauth(name: string) {
    setOauthWorking(name);
    setError(null);
    try {
      await client.disconnectMcpServer(name);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setOauthWorking(null);
    }
    await refreshServers();
  }

  /** Add one directory server, then start its sign-in when it asks for one.
   * The add saves only that server, so unsaved edits stay unsaved: the new
   * row joins the draft, and the next save keeps it. */
  async function addFromDirectory(entry: McpDirectoryEntry) {
    setAdding(entry.id);
    setAddError(null);
    let signIn: string | null = null;
    try {
      const result = await client.addMcpDirectoryServer(entry.id);
      // Supersede any in-flight background read; this list is fresher.
      requestRef.current += 1;
      rememberSaved(result.servers);
      const added = result.servers.find(
        (server) => server.name === result.name,
      );
      if (!dirtyRef.current) {
        setServers(result.servers);
      } else if (added !== undefined) {
        setServers((current) =>
          current.some((server) => server.name === added.name)
            ? current
            : [...current, added],
        );
      }
      setServersKnown(true);
      setListError(null);
      // A window attached to another machine cannot finish a sign-in, so it
      // leaves Connect to the row, which says where the sign-in has to run.
      if (
        added?.oauth_status?.state === "not_connected" &&
        !attachedRemotely()
      ) {
        signIn = result.name;
      } else {
        toast.success(`Added ${entry.name}`);
      }
    } catch (err) {
      setAddError(`Could not add ${entry.name}: ${errorMessage(err)}`);
    } finally {
      setAdding(null);
    }
    if (signIn !== null) await connectOauth(signIn);
  }

  // While a sign-in waits on the browser, while the server reconnects after
  // one, and while a saved server makes its first connection after
  // Tidebreak starts, read the list often enough that each row settles on
  // its own, without a manual refresh.
  const following = servers.some(
    (server) =>
      server.oauth_status?.state === "authorizing" ||
      (server.oauth_status?.state === "connected" && settling(server.health)) ||
      (savedNames.has(server.name) && settling(server.health)),
  );
  useEffect(() => {
    if (!following) return;
    const timer = window.setInterval(
      () => void refreshServers(),
      SIGN_IN_POLL_MS,
    );
    return () => window.clearInterval(timer);
    // refreshServers reads only refs, the client, and state setters.
  }, [client, following]);

  const working =
    saving ||
    importing ||
    reconnecting !== null ||
    oauthWorking !== null ||
    adding !== null ||
    mounting;

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
      Could not read the MCP server list: {listError}
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
            Sign in to the Model Gateway to connect or disconnect endpoints. The
            configured connections stay listed meanwhile.
          </p>
        )}
        {appsFailed && (
          <p className="text-muted-foreground text-xs">
            Could not read your entitlements from the gateway; these are the
            configured connections.
          </p>
        )}
        {listError !== null && (
          <div className="flex items-center justify-between gap-4">
            <SettingsError>
              Could not read the MCP server list: {listError}
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
              Try again
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
                    connected
                    <Switch
                      aria-label={`Connect ${slug}`}
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
                      Connection state unknown.
                    </span>
                  )}
                  {revoked && (
                    <span className="text-xs text-muted-foreground">
                      No longer granted to your teams. Switch off to disconnect
                      it.
                    </span>
                  )}
                  {mounted &&
                    mounted.health !== "healthy" &&
                    mounted.health !== "initializing" &&
                    mounted.health !== "reconnecting" &&
                    mounted.diagnostic !== null && (
                      <span className="text-xs text-destructive break-words">
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
      description={`Connect stdio tool servers that run on ${hostMachineLabel()}, or remote HTTP endpoints, without a shell or a desktop restart.`}
      busy={loading || working}
    >
      {endpointsSection}
      {loading ? (
        <p className="text-sm text-muted-foreground">Loading MCP servers…</p>
      ) : (
        <>
          <McpDirectoryList
            servers={directory}
            configured={servers}
            adding={adding}
            disabled={working}
            loadError={directoryError}
            addError={addError}
            onAdd={(entry) => void addFromDirectory(entry)}
          />

          <SettingsSection
            title="Import configuration"
            description="Add servers from a Tidebreak, Claude, Cursor, Windsurf, or VS Code JSON file, or paste that JSON here. Imported servers stay unsaved until you review them and save."
          >
            <div className="flex flex-col items-start gap-2">
              <SettingsField
                label="MCP JSON"
                hint="Paste a Claude Desktop, Cursor, Windsurf, VS Code, or Tidebreak MCP file. Failed imports keep this text so you can fix it."
              >
                <textarea
                  className="min-h-40 w-full rounded-md border bg-transparent p-2 font-mono text-xs"
                  value={importDraft}
                  disabled={working}
                  spellCheck={false}
                  aria-label="MCP JSON"
                  placeholder='{ "mcpServers": { "docs": { "command": "npx" } } }'
                  onChange={(event) => setImportDraft(event.target.value)}
                />
              </SettingsField>
              <div className="flex flex-wrap gap-2">
                <input
                  ref={importInputRef}
                  type="file"
                  accept=".json,application/json"
                  className="sr-only"
                  aria-label="Import MCP configuration"
                  disabled={working}
                  onChange={(event) => {
                    const input = event.currentTarget;
                    const file = input.files?.[0];
                    if (file !== undefined) void importConfiguration(file);
                    input.value = "";
                  }}
                />
                <Button
                  type="button"
                  variant="outline"
                  disabled={working}
                  onClick={() => importInputRef.current?.click()}
                >
                  <Upload size={14} />
                  {importing ? "Importing…" : "Import JSON file"}
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  disabled={working || importDraft.trim().length === 0}
                  onClick={() => importPastedJson()}
                >
                  Import pasted JSON
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                Tidebreak imports environment variable names but never their
                values. Enter secret values again before you save.
              </p>
            </div>
            {importSummary !== null && (
              <ImportSummary summary={importSummary} />
            )}
            {importError !== null && (
              <SettingsError>{importError}</SettingsError>
            )}
          </SettingsSection>

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
                <McpServerSummary
                  server={server}
                  busy={oauthWorking === server.name}
                  disabled={working && oauthWorking !== server.name}
                  remote={attachedRemotely()}
                  saved={savedNames.has(server.name)}
                  onConnect={() => void connectOauth(server.name)}
                  onDisconnect={() => void disconnectOauth(server.name)}
                  onCancel={() => void cancelOauth(server.name)}
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
                    session; connect or disconnect it under Gateway endpoints
                    above.
                  </p>
                )}

                {transportOf(server) !== "gateway" && (
                  <FieldGroup label="Transport">
                    <RadioGroup
                      className="flex flex-row flex-wrap gap-4"
                      value={transportOf(server)}
                      aria-label="Transport"
                      disabled={working}
                      onValueChange={(transport) =>
                        update(
                          index,
                          transportFields(transport as "stdio" | "http"),
                        )
                      }
                    >
                      {(["stdio", "http"] as const).map((transport) => (
                        <Label
                          key={transport}
                          className="flex items-center gap-2 text-sm font-normal"
                        >
                          <RadioGroupItem value={transport} />
                          {transport === "stdio"
                            ? `Process on ${hostMachineLabel()} (stdio)`
                            : "Remote endpoint (HTTP)"}
                        </Label>
                      ))}
                    </RadioGroup>
                  </FieldGroup>
                )}

                {transportOf(server) === "stdio" && (
                  <>
                    <SettingsField
                      label="Executable"
                      hint={`A command name or absolute path on ${hostMachineLabel()}. A bare name is resolved on the process PATH extended with the login-shell PATH (and PATHEXT on Windows) without invoking a shell. Relative paths with separators are refused.`}
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
                    {server.resolved_command &&
                    server.command &&
                    server.resolved_command !== server.command ? (
                      <p className="text-sm text-muted-foreground">
                        Resolved{" "}
                        <span className="font-mono">{server.command}</span> to{" "}
                        <span className="font-mono">
                          {server.resolved_command}
                        </span>
                      </p>
                    ) : null}

                    <StringListEditor
                      label="Arguments"
                      values={server.args}
                      disabled={working}
                      addLabel="Add argument"
                      onChange={(args) => update(index, { args })}
                    />

                    <SettingsField
                      label="Working directory"
                      hint="Optional. It does not grant the server any Tidebreak folder capability."
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
                      hint="Optional. Leave it blank for a server you sign in to with Connect. Tidebreak reads this variable from the process environment it started with and never displays the value. Export it in the shell you start Tidebreak from, then restart Tidebreak. A Dock or Finder launch does not see variables from your shell profile."
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
                      hint="Only names are saved or displayed. Tidebreak reads their values from the process environment it started with. Export them in the shell you start Tidebreak from, then restart Tidebreak."
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
                      Disconnect
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
            <Button
              type="button"
              disabled={working}
              onClick={() => void save()}
            >
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
            OS credential store and never come back to this window; do not enter
            secrets in the executable, arguments, working directory, or a server
            URL, which are ordinary settings.
          </p>
        </>
      )}
      {fallbackListError}
      {error && <SettingsError>{error}</SettingsError>}
    </McpKindSection>
  );
}

function ImportSummary({ summary }: { summary: McpImportSummary }) {
  const imported = summary.servers.length;
  const skipped = summary.skipped.length;
  const secrets = secretsByServer(summary.secrets);
  return (
    <div className="flex min-w-0 flex-col gap-2">
      <p className="min-w-0 text-sm" role="status" aria-atomic="true">
        {imported === 0 ? (
          <>
            No servers imported from{" "}
            <span className="break-all">{summary.fileName}</span>.
          </>
        ) : (
          <>
            {imported} server{imported === 1 ? "" : "s"} added to the editor
            from <span className="break-all">{summary.fileName}</span>.
          </>
        )}{" "}
        {skipped > 0 &&
          `${skipped} entr${skipped === 1 ? "y was" : "ies were"} skipped.`}
      </p>
      {secrets.length > 0 && (
        <div className="text-xs text-muted-foreground">
          <p>Enter these environment values before saving:</p>
          <ul
            aria-label="Environment values to enter"
            className="mt-1 list-disc space-y-1 pl-5"
          >
            {secrets.map(([server, names]) => (
              <li key={server} className="min-w-0 break-words">
                <code className="break-all">{server}</code>:{" "}
                {names.map((name, index) => (
                  <span key={name}>
                    {index > 0 && ", "}
                    <code className="break-all">{name}</code>
                  </span>
                ))}
              </li>
            ))}
          </ul>
        </div>
      )}
      {summary.skipped.length > 0 && (
        <ul
          aria-label="Skipped MCP servers"
          className="list-disc space-y-1 pl-5 text-xs text-muted-foreground"
        >
          {summary.skipped.map((item, index) => (
            <li key={`${item.name}-${index}`} className="min-w-0 break-words">
              <code className="break-all">{item.name}</code>: {item.reason}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function secretsByServer(
  secrets: McpImportSecret[],
): Array<[string, string[]]> {
  const grouped = new Map<string, string[]>();
  for (const secret of secrets) {
    const names = grouped.get(secret.server) ?? [];
    if (!names.includes(secret.name)) names.push(secret.name);
    grouped.set(secret.server, names);
  }
  return [...grouped.entries()];
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
function verifyDescription(server: McpServerInfo): string {
  if (server.health === "healthy") {
    return `${server.tool_count} tool${server.tool_count === 1 ? "" : "s"} available to new turns.`;
  }
  if (server.health === "disabled") {
    return (
      server.diagnostic ??
      "Disabled. This server is not available to new turns."
    );
  }
  return server.diagnostic ?? "Save the configuration to verify this server.";
}

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
 * class prefix ("HttpError: ...") in front of it. HTTP status prefixes stay
 * off so a rejected save shows the server's field-level reason. */
function errorMessage(err: unknown): string {
  if (err instanceof HttpError) {
    return err.message.replace(/^\d+:\s*/, "");
  }
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
    oauth: false,
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
    if (!serversKnown) return "Connection state unknown.";
    if (entitledSlugs !== null && !entitledSlugs.has(slug)) {
      return "No longer granted to your teams. Switch off to disconnect it.";
    }
    return mounted ? mountStatus(mounted) : null;
  };

  return (
    <SettingsSection
      title="Gateway endpoints"
      description="Connected endpoints use your gateway session — no tokens to copy, and they reconnect after you sign back in."
    >
      {!signedIn && (
        <p className="text-muted-foreground text-xs">
          Sign in to the Model Gateway to connect or disconnect endpoints. The
          configured connections stay listed meanwhile.
        </p>
      )}
      {appsFailed && (
        <p className="text-muted-foreground text-xs">
          Could not read your entitlements from the gateway; these are the
          configured connections.
        </p>
      )}
      {listError !== null && (
        <div className="flex items-center justify-between gap-4">
          <SettingsError>
            Could not read the MCP server list: {listError}
          </SettingsError>
          <Button
            type="button"
            variant="outline"
            disabled={working}
            onClick={onRetry}
          >
            <RefreshCw size={14} />
            Try again
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
                  aria-label={`Connect ${slug}`}
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
          <div
            className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] gap-2"
            key={index}
          >
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
              onChange={(event) => replaceRow(index, name, event.target.value)}
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

/** A server that failed is critical; one still connecting or never verified
 * is not an error yet. */
function healthTone(health: McpServerInfo["health"]): SettingsStatusTone {
  switch (health) {
    case "healthy":
      return "ready";
    case "degraded":
      return "critical";
    case "reconnecting":
      return "warning";
    case "initializing":
    case "disabled":
      return "neutral";
  }
}
