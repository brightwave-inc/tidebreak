import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import {
  ExternalLink,
  Laptop,
  LogOut,
  PlugZap,
  RefreshCw,
  Server,
} from "lucide-react";
import type {
  ApiClient,
  GatewayApps,
  GatewayStatus,
  RemoteMachineState,
} from "../api";
import { openInBrowser } from "../openInBrowser";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { useConfirm } from "@/components/ConfirmDialog";
import {
  HOST_AUTHORITIES,
  connectGatewayRemoteMachine,
  connectFailureMessage,
  connectRemoteMachine,
  disconnectRemoteMachine,
  hostAuthorityLabel,
  remoteConnectError,
  remoteMachineState,
} from "@/remoteMachine";
import { friendlyErrorMessage } from "@/lib/utils";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
  usedByLabel,
} from "./primitives";

const SIGN_IN_POLL_MS = 2_000;

/**
 * How the panel reaches the machine this window is attached to.
 *
 * The shell owns the address and the token, so these are native commands.
 * Passing them in is what lets a story render every attachment state without
 * a native shell behind it.
 */
export type MachineControls = {
  read: () => Promise<RemoteMachineState>;
  attachWithGateway: (baseUrl: string) => Promise<unknown>;
  attachWithToken: (baseUrl: string, token: string) => Promise<unknown>;
  detach: () => Promise<unknown>;
  /**
   * Rebuild this window against the machine that is now current.
   *
   * A reload rather than a state update: the API client, the event stream,
   * and every cached read were built against the machine this window opened
   * on. Rebuilding them piecemeal is how a client ends up half-attached.
   */
  reattach: () => void;
};

const NATIVE_MACHINE: MachineControls = {
  read: remoteMachineState,
  attachWithGateway: connectGatewayRemoteMachine,
  attachWithToken: connectRemoteMachine,
  detach: disconnectRemoteMachine,
  reattach: () => window.location.reload(),
};

/**
 * Readiness copy for one entitled app, from the gateway's member catalog.
 * `ready` (and an absent value, on gateways that predate the catalog) render
 * nothing; the named not-ready states get their own copy; anything
 * unfamiliar renders as generic not-ready copy — the state set is the
 * gateway's to grow, and an unknown value must degrade, not break.
 */
function appReadinessLabel(connection: string | undefined): string | null {
  switch (connection) {
    case undefined:
    case "ready":
      return null;
    case "not_connected":
      return "connect this app at your gateway to use it";
    case "authorization_required":
      return "authorize this app at your gateway to use it";
    default:
      return "not ready at your gateway";
  }
}

/**
 * The Model Gateway section: who governs this profile, and which machine its
 * work runs on.
 *
 * Policy is the only gateway source: a profile connects through the
 * gateway's own page (deep-link pairing) or the organization's device
 * management, never from settings — there is no URL field and no enable
 * toggle in any state. Unmanaged profiles read a signpost at that flow;
 * managed profiles get the slim identity panel: who is signed in, the
 * read-only gateway origin from policy, sign in/out, and an explicit
 * gateway sync (models and MCP endpoint mounts together).
 *
 * The machine lives here and not in a section of its own because the two
 * questions are one: an organization's gateway hosts the machine it offers,
 * and attaching to it is the same decision as being governed by it. Every
 * profile sees the machine sections, including an unmanaged one — a machine
 * behind no gateway is reachable with its own token, and this is the only
 * place in the app that reaches it.
 *
 * A gateway-authenticated hosted machine is the third state, and it has no
 * session of its own to show: the reader signed in to the gateway on their
 * own computer and attached with that account, so this names the gateway and
 * leaves the machine sections to say where the work runs.
 */
export function GatewayPanel({
  client,
  managed,
  gatewayUrl,
  hostedGatewayUrl = null,
  onChanged,
  onOpenConnectedApps,
  machine = NATIVE_MACHINE,
}: {
  client: ApiClient;
  /** Whether the resolved policy manages this profile. */
  managed: boolean;
  /** The policy's locked gateway origin, shown read-only. */
  gatewayUrl: string | null;
  /** The gateway the machine this window works on authenticates its callers
   * against. Never a session this profile holds — see the hosted panel. */
  hostedGatewayUrl?: string | null;
  onChanged: () => void;
  /** Navigates to the Connected apps page, whose MCP section is where
   * entitled endpoints are mounted. Mounting lives beside the health of what
   * is mounted, so this panel points at it rather than carrying its own
   * toggles. */
  onOpenConnectedApps: () => void;
  machine?: MachineControls;
}) {
  if (!managed && hostedGatewayUrl !== null) {
    // Read-only, with no sign in and no sign out, because there is nothing
    // here to sign in to: the reader authenticated to this machine with their
    // own gateway account, and the machine holds that credential for as long
    // as the session lasts. A Connect button would start a flow this machine
    // could never complete.
    return (
      <SettingsPanel
        title="Model Gateway"
        description="This machine authenticates you with your Model Gateway account."
      >
        <p className="text-sm">
          <code className="font-medium">{hostedGatewayUrl}</code>
        </p>
        <p className="text-sm leading-relaxed text-muted-foreground">
          You signed in to this gateway on your own computer, and this machine
          runs your work with that account. Models and usage follow your
          entitlements there, not this machine&apos;s. Your access ends here the
          moment it ends at the gateway.
        </p>
        <MachineSections client={client} managed={false} machine={machine} />
      </SettingsPanel>
    );
  }
  if (!managed) {
    return (
      <SettingsPanel
        title="Model Gateway"
        description="This profile is not connected to a model gateway."
      >
        <p className="text-sm leading-relaxed text-muted-foreground">
          Connecting happens from your gateway&apos;s own page — open it in your
          browser and choose Connect — or through your organization&apos;s
          device management. There is nothing to configure here; until then,
          Tidebreak stays fully local with your own provider keys.
        </p>
        <MachineSections client={client} managed={false} machine={machine} />
      </SettingsPanel>
    );
  }
  return (
    <ManagedGatewayPanel
      client={client}
      gatewayUrl={gatewayUrl}
      onChanged={onChanged}
      onOpenConnectedApps={onOpenConnectedApps}
      machine={machine}
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
  machine,
}: {
  client: ApiClient;
  gatewayUrl: string | null;
  onChanged: () => void;
  onOpenConnectedApps: () => void;
  machine: MachineControls;
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
    status.sign_in.state === "pending"
      ? status.sign_in.authorization_url
      : null;
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
            {status.model_count > 0 && !status.member_catalog && (
              // Soft note, never a block: an older gateway still signs in
              // and still syncs models; what it cannot do is report app
              // readiness or serve instant catalog updates.
              <p className="text-muted-foreground text-xs">
                This gateway is older than this Tidebreak. Models still sync,
                but app readiness and instant catalog updates are unavailable
                until the gateway is upgraded.
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
                    toast.success(
                      "Synced models and MCP endpoints from the gateway",
                    );
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
                      {appReadinessLabel(app.connection) && (
                        <span className="text-muted-foreground text-xs">
                          {appReadinessLabel(app.connection)}
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

      <MachineSections client={client} managed machine={machine} />
    </SettingsPanel>
  );
}

/**
 * Which machine this window works on.
 *
 * A machine is a running Tidebreak server. This app ships with one inside it,
 * which is what you use by default. Attaching to another one means your
 * conversations, your agents, and their work live there instead — so they keep
 * running when you close this laptop, and any device you hold can supervise
 * them.
 *
 * The trade is host authority, and the panel states it up front rather than
 * letting you discover it when a folder picker refuses.
 */
function MachineSections({
  client,
  managed,
  machine,
}: {
  client: ApiClient;
  managed: boolean;
  machine: MachineControls;
}) {
  const [state, setState] = useState<RemoteMachineState | null>(null);
  const [baseUrl, setBaseUrl] = useState("");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [offered, setOffered] = useState<string | null>(null);
  const { confirm, dialog: confirmDialog } = useConfirm();

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const current = await machine.read();
        if (!cancelled) setState(current);
      } catch (failure) {
        if (!cancelled)
          setError(
            friendlyErrorMessage(failure, "Could not read the connection."),
          );
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [machine]);

  // The gateway's own answer to which machine it hosts, read from its
  // metadata rather than from the provision link — the link fires once at
  // pair time, so a profile paired earlier would never see it. Reading the
  // offer never attaches: attaching moves your work to another machine and
  // takes away connected folders, computer use, and native export, so it
  // stays something you choose. An absent value, an older gateway, and an
  // unreachable one are one case here: no prefill.
  useEffect(() => {
    if (!managed) return;
    let cancelled = false;
    void (async () => {
      try {
        const offer = await client.getGatewayMachine();
        const url = offer.url;
        if (cancelled || !url) return;
        setOffered(url);
        // Never over an address the reader is already typing.
        setBaseUrl((current) => (current === "" ? url : current));
      } catch {
        // A hint that cannot be read is simply no hint.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, managed]);

  const remote = state?.attachment === "remote";

  async function attach(run: () => Promise<unknown>, fallback: string) {
    setBusy(true);
    setError(null);
    try {
      await run();
      machine.reattach();
    } catch (failure) {
      const refused = remoteConnectError(failure);
      setError(
        refused
          ? connectFailureMessage(refused)
          : friendlyErrorMessage(failure, fallback),
      );
      setBusy(false);
    }
  }

  async function detach() {
    const accepted = await confirm({
      title: "Work on this computer again?",
      description:
        "This window reattaches to the Tidebreak server inside this app. Work on the other machine keeps running; you stop watching it, and this computer's copy of the token is forgotten.",
      confirmLabel: "Disconnect",
    });
    if (!accepted) return;
    setBusy(true);
    setError(null);
    try {
      await machine.detach();
      machine.reattach();
    } catch (failure) {
      setError(friendlyErrorMessage(failure, "Could not disconnect."));
      setBusy(false);
    }
  }

  return (
    <>
      {confirmDialog}
      <SettingsSection
        title="Machine"
        description="Which Tidebreak server this window works on."
      >
        {remote ? (
          <SettingsStatus
            tone="ready"
            label="Attached to a remote machine"
            description={
              <>
                Your conversations and agents run on {state?.baseUrl}. They keep
                running when you close this window.
              </>
            }
          />
        ) : (
          <SettingsStatus
            tone="disabled"
            label="Working on this computer"
            description="Your conversations and agents run inside this app, and stop when it does."
          />
        )}

        {remote ? (
          <>
            <SettingsField label="Address">
              <p className="flex items-center gap-2 text-sm">
                <Server size={16} aria-hidden />
                <span>{state?.baseUrl}</span>
              </p>
            </SettingsField>
            <p className="text-sm text-muted-foreground">
              Disconnecting returns this window to the server inside this app.
              It changes nothing on the remote machine.
            </p>
            <div>
              <Button
                variant="outline"
                onClick={() => void detach()}
                disabled={busy}
              >
                <Laptop size={16} aria-hidden />
                Work on this computer
              </Button>
            </div>
          </>
        ) : (
          <>
            <SettingsField
              label="Address"
              hint={
                offered
                  ? "Offered by your gateway. Change it to reach a different machine."
                  : "For example, https://tidebreak.example.com."
              }
            >
              <Input
                value={baseUrl}
                onChange={(event) => setBaseUrl(event.target.value)}
                placeholder="https://"
                autoComplete="off"
                spellCheck={false}
                disabled={busy}
              />
            </SettingsField>
            <div>
              <Button
                onClick={() =>
                  void attach(
                    () => machine.attachWithGateway(baseUrl),
                    "Could not connect through Model Gateway.",
                  )
                }
                disabled={busy || !baseUrl.trim()}
              >
                <Server size={16} aria-hidden />
                Connect with Model Gateway
              </Button>
            </div>
            <Collapsible>
              <CollapsibleTrigger asChild>
                <Button
                  type="button"
                  variant="link"
                  className="h-auto px-0 text-sm text-muted-foreground hover:text-foreground"
                >
                  Advanced
                </Button>
              </CollapsibleTrigger>
              <CollapsibleContent className="flex flex-col gap-4 pt-4">
                <SettingsField
                  label="Standalone token"
                  hint="Only for a Tidebreak machine that is not connected to Model Gateway. Its operator issues the token."
                >
                  <Input
                    type="password"
                    value={token}
                    onChange={(event) => setToken(event.target.value)}
                    autoComplete="off"
                    spellCheck={false}
                    disabled={busy}
                  />
                </SettingsField>
                <div>
                  <Button
                    onClick={() =>
                      void attach(
                        () => machine.attachWithToken(baseUrl, token),
                        "Could not connect to that machine.",
                      )
                    }
                    disabled={busy || !baseUrl.trim() || !token.trim()}
                  >
                    <Server size={16} aria-hidden />
                    Connect with token
                  </Button>
                </div>
              </CollapsibleContent>
            </Collapsible>
          </>
        )}
        {error && <SettingsError>{error}</SettingsError>}
      </SettingsSection>

      <SettingsSection
        title="What stays on this computer"
        description={
          remote
            ? "These reach this computer's files, screen, and input. Your conversation is not on this computer, so they are unavailable until you disconnect."
            : "These reach this computer's files, screen, and input. Attaching to a remote machine makes them unavailable, because your conversation would not be on this computer."
        }
      >
        <ul className="flex flex-col gap-2 text-sm">
          {HOST_AUTHORITIES.map((authority) => (
            <li key={authority} className="flex items-center gap-2">
              <Laptop
                size={16}
                aria-hidden
                className={remote ? "text-muted-foreground" : "text-icon-green"}
              />
              <span
                className={
                  remote ? "text-muted-foreground line-through" : undefined
                }
              >
                {hostAuthorityLabel(authority)}
              </span>
            </li>
          ))}
        </ul>
      </SettingsSection>
    </>
  );
}
