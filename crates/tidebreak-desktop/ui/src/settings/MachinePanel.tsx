import { useEffect, useState } from "react";
import { Laptop, Server } from "lucide-react";

import type { RemoteMachineState } from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
} from "./primitives";

/**
 * Where you choose which machine this window works on.
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
export function MachinePanel() {
  const [state, setState] = useState<RemoteMachineState | null>(null);
  const [baseUrl, setBaseUrl] = useState("");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { confirm, dialog: confirmDialog } = useConfirm();

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const current = await remoteMachineState();
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
  }, []);

  const remote = state?.attachment === "remote";

  /**
   * A reload rather than a state update: the API client, the event stream, and
   * every cached read were built against the machine this window opened on.
   * Rebuilding them piecemeal is how a client ends up half-attached.
   */
  function reattach() {
    window.location.reload();
  }

  async function connect() {
    setBusy(true);
    setError(null);
    try {
      await connectRemoteMachine(baseUrl, token);
      reattach();
    } catch (failure) {
      const refused = remoteConnectError(failure);
      setError(
        refused
          ? connectFailureMessage(refused)
          : friendlyErrorMessage(failure, "Could not connect to that machine."),
      );
      setBusy(false);
    }
  }

  async function connectWithGateway() {
    setBusy(true);
    setError(null);
    try {
      await connectGatewayRemoteMachine(baseUrl);
      reattach();
    } catch (failure) {
      const refused = remoteConnectError(failure);
      setError(
        refused
          ? connectFailureMessage(refused)
          : friendlyErrorMessage(
              failure,
              "Could not connect through Model Gateway.",
            ),
      );
      setBusy(false);
    }
  }

  async function disconnect() {
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
      await disconnectRemoteMachine();
      reattach();
    } catch (failure) {
      setError(friendlyErrorMessage(failure, "Could not disconnect."));
      setBusy(false);
    }
  }

  return (
    <SettingsPanel
      title="Machine"
      description="Which Tidebreak server this window works on."
      busy={busy}
    >
      {confirmDialog}
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
        <SettingsSection
          title="Remote machine"
          description="Disconnecting returns this window to the server inside this app. It changes nothing on the remote machine."
        >
          <SettingsField label="Address">
            <p className="flex items-center gap-2 text-sm">
              <Server size={16} aria-hidden />
              <span>{state?.baseUrl}</span>
            </p>
          </SettingsField>
          <div>
            <Button
              variant="outline"
              onClick={() => void disconnect()}
              disabled={busy}
            >
              <Laptop size={16} aria-hidden />
              Work on this computer
            </Button>
          </div>
          {error && <SettingsError>{error}</SettingsError>}
        </SettingsSection>
      ) : (
        <SettingsSection
          title="Connect to a machine"
          description="Enter the hosted Tidebreak address. If it uses the same Model Gateway as this app, your existing sign-in supplies access automatically."
        >
          <SettingsField
            label="Address"
            hint="For example, https://tidebreak.example.com."
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
              onClick={() => void connectWithGateway()}
              disabled={busy || !baseUrl.trim()}
            >
              <Server size={16} aria-hidden />
              Connect with Model Gateway
            </Button>
          </div>
          <SettingsField
            label="Standalone token"
            hint="Only for a Tidebreak machine that is not connected to Model Gateway."
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
              onClick={() => void connect()}
              disabled={busy || !baseUrl.trim() || !token.trim()}
            >
              <Server size={16} aria-hidden />
              Connect with token
            </Button>
          </div>
          {error && <SettingsError>{error}</SettingsError>}
        </SettingsSection>
      )}

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
    </SettingsPanel>
  );
}
