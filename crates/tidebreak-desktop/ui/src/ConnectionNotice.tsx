import { Laptop, RotateCw } from "lucide-react";
import { toast } from "sonner";

import { useOptionalApp } from "@/AppContext";
import { Button } from "@/components/ui/button";
import { Notice, NoticeRetryButton } from "@/components/ui/notice";
import { Spinner } from "@/components/ui/spinner";
import { useRetry } from "@/components/ui/useRetry";
import { friendlyErrorMessage } from "@/lib/utils";
import {
  CONNECTION_ESCALATE_AFTER_MS,
  machineHost,
  useConnectionIndicator,
  useServerHealth,
  type SocketConnectionState,
} from "./connectionState";
import { restartTidebreak } from "./desktopLifecycle";
import { hasNativeHost } from "./host";
import { disconnectRemoteMachine } from "./remoteMachine";

/** What the connection notice says. */
export type ConnectionNoticeState = "reconnecting" | "escalated" | "stopped";

export type ConnectionNoticeProps = {
  state: ConnectionNoticeState;
  /** The other machine's host, when this window works on one. */
  machine?: string | null;
  /** Reconnect now; resolves when the attempt settles. */
  onRetryNow?: () => Promise<void> | void;
  /** Leave the other machine and work on this computer. Remote only. */
  onWorkLocally?: () => Promise<void> | void;
  /** Quit and reopen Tidebreak, through the quit prompt. */
  onRestart?: () => Promise<void> | void;
  className?: string;
};

const ESCALATE_AFTER_SECONDS = CONNECTION_ESCALATE_AFTER_MS / 1_000;

/**
 * A lost connection to the server, where the reader is about to type: above
 * the composer in a conversation and in a code session alike.
 *
 * A drop is quiet at first, because most come back within seconds. One that
 * lasts escalates to a notice with Retry now, and on another machine with a
 * way back to this one. A local server that stopped outright says so, since
 * nothing will reconnect to it until Tidebreak restarts.
 */
export function ConnectionNotice({
  state,
  machine = null,
  onRetryNow,
  onWorkLocally,
  onRestart,
  className,
}: ConnectionNoticeProps) {
  const retry = useRetry(() => onRetryNow?.());
  const detach = useRetry(() => onWorkLocally?.());
  const restart = useRetry(() => onRestart?.());

  if (state === "reconnecting") {
    return (
      <Notice
        tone="info"
        density="compact"
        icon={<Spinner className="size-3.5 text-info" />}
        className={className}
      >
        Reconnecting to Tidebreak…
      </Notice>
    );
  }

  if (state === "stopped") {
    return (
      <Notice
        tone="critical"
        title="Tidebreak's server stopped"
        className={className}
        action={
          onRestart && (
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={restart.pending}
              onClick={restart.retry}
            >
              {restart.pending ? (
                <Spinner aria-hidden="true" />
              ) : (
                <RotateCw aria-hidden="true" />
              )}
              Restart Tidebreak
            </Button>
          )
        }
      >
        Restart Tidebreak to start it again. Your conversations are saved.
      </Notice>
    );
  }

  return (
    <Notice
      tone="warning"
      title={
        machine ? `${machine} is not answering` : "Tidebreak is not answering"
      }
      className={className}
      action={
        <>
          {onRetryNow && (
            <NoticeRetryButton pending={retry.pending} onClick={retry.retry}>
              Retry now
            </NoticeRetryButton>
          )}
          {machine && onWorkLocally && (
            <Button
              type="button"
              size="sm"
              variant="ghost"
              disabled={detach.pending}
              onClick={detach.retry}
            >
              <Laptop aria-hidden="true" />
              Work on this computer
            </Button>
          )}
        </>
      }
    >
      {machine
        ? `The connection dropped over ${ESCALATE_AFTER_SECONDS} seconds ago, and Tidebreak keeps trying. To keep going now, work on this computer. Nothing changes on that machine.`
        : `The connection dropped over ${ESCALATE_AFTER_SECONDS} seconds ago. Tidebreak keeps trying, and your conversations are saved.`}
    </Notice>
  );
}

/**
 * Forget the other machine and open this computer's Tidebreak. The window
 * reloads, as the Machine settings panel's does: the API client and every
 * socket were built against the machine it opened on.
 */
async function workOnThisComputer(): Promise<void> {
  try {
    await disconnectRemoteMachine();
  } catch (error) {
    toast.error(
      friendlyErrorMessage(error, "Could not return to this computer."),
    );
    return;
  }
  window.location.reload();
}

/**
 * The connection notice for one socket, with what the shell knows filled
 * in: which machine the window works on, whether the local server stopped,
 * and the way back to this computer. Renders nothing while the socket is
 * live.
 */
export function ConnectionStatus({
  connection,
  onRetryNow,
  className,
}: {
  connection: SocketConnectionState;
  onRetryNow: () => Promise<void>;
  className?: string;
}) {
  const app = useOptionalApp();
  const indicator = useConnectionIndicator(connection);
  const serverStopped = useServerHealth((health) => health.stopped);
  const remote = app?.attachment === "remote";
  const state: ConnectionNoticeState | null =
    serverStopped && !remote
      ? "stopped"
      : indicator === "live"
        ? null
        : indicator;
  if (!state) return null;
  return (
    <ConnectionNotice
      state={state}
      machine={remote && app ? machineHost(app.client.baseUrl) : null}
      onRetryNow={onRetryNow}
      // Only the desktop shell holds an attachment it can forget; a browser
      // tab on a hosted machine has no computer of its own to return to.
      onWorkLocally={remote && hasNativeHost() ? workOnThisComputer : undefined}
      onRestart={restartTidebreak}
      className={className}
    />
  );
}
