import { invoke, isTauri } from "@tauri-apps/api/core";

import type {
  RemoteConnectError,
  RemoteConnectReason,
  RemoteMachineState,
} from "./api";
import type { HostAuthority } from "./host";
import { attachedRemotely, hostAuthorityRefusal } from "./host";
import { hostedSession } from "./hostedSession";
import { friendlyErrorMessage } from "./lib/utils";

/**
 * Attaching this client to a machine it does not host.
 *
 * A machine is a running Tidebreak server. Normally this app is both machine
 * and client. Attached remotely, it is only the client: the conversation, the
 * agents, and the work all live somewhere else, and this computer contributes
 * nothing but the window.
 *
 * The shell owns the address and the token. This module owns the copy — every
 * refusal crosses the boundary as a stable reason, never as a sentence, so the
 * wording below is the renderer's to change without touching the shell.
 */

/**
 * Which machine this client is attached to.
 *
 * Outside the native shell the answer is the page's: a tab served by a
 * machine is attached to that machine, and any other browser tab is local.
 */
export async function remoteMachineState(): Promise<RemoteMachineState> {
  if (!isTauri()) {
    const hosted = hostedSession();
    return hosted
      ? { attachment: "remote", baseUrl: hosted.baseUrl }
      : { attachment: "local", baseUrl: null };
  }
  return await invoke<RemoteMachineState>("remote_machine_state");
}

/** Attach to a machine. Throws a {@link RemoteConnectError} when refused. */
export async function connectRemoteMachine(
  baseUrl: string,
  token: string,
): Promise<RemoteMachineState> {
  return await invoke<RemoteMachineState>("connect_remote_machine", {
    baseUrl,
    token,
  });
}

/** Attach using the Model Gateway session this desktop already holds. */
export async function connectGatewayRemoteMachine(
  baseUrl: string,
): Promise<RemoteMachineState> {
  return await invoke<RemoteMachineState>("connect_gateway_remote_machine", {
    baseUrl,
  });
}

/** Mint or retrieve a fresh bearer for the currently attached machine. */
export async function remoteMachineAccessToken(): Promise<string> {
  return await invoke<string>("remote_machine_access_token");
}

/** Detach and forget the machine's token. */
export async function disconnectRemoteMachine(): Promise<RemoteMachineState> {
  return await invoke<RemoteMachineState>("disconnect_remote_machine");
}

/** A refused connect, or `null` when the failure was something else. */
export function remoteConnectError(error: unknown): RemoteConnectError | null {
  if (!error || typeof error !== "object") return null;
  const candidate = error as { reason?: unknown; detail?: unknown };
  if (typeof candidate.reason !== "string") return null;
  if (!(candidate.reason in CONNECT_COPY)) return null;
  return {
    reason: candidate.reason as RemoteConnectReason,
    detail: typeof candidate.detail === "string" ? candidate.detail : null,
  };
}

/**
 * What to say about each refusal.
 *
 * Exhaustive by construction: a new reason on the shell side has no entry here
 * until someone writes one, and `remoteConnectError` will not recognize it, so
 * an unworded refusal surfaces as an ordinary failure rather than as a blank.
 */
const CONNECT_COPY: Record<RemoteConnectReason, string> = {
  remote_machine_url_invalid:
    "That is not an address this app can use. Enter the full URL, starting with https://.",
  remote_machine_requires_tls:
    "The address must use https. Plain http reaches only a machine on this computer, because the token would otherwise travel in the clear.",
  remote_machine_unreachable:
    "That machine did not answer. Check the address, and check that you can reach it from this network.",
  remote_machine_token_refused:
    "That machine refused the token. Check that you copied all of it, and that it has not been revoked.",
  remote_machine_not_a_machine:
    "Something answered at that address, but not a Tidebreak machine. Check the address.",
  remote_machine_token_storage_failed:
    "The token could not be saved to this computer's credential store. Nothing was changed.",
  remote_machine_gateway_auth_unavailable:
    "This machine is not connected to the same Model Gateway as this app. Sign in through that Gateway, or use a static token for a standalone machine.",
};

export function connectFailureMessage(error: RemoteConnectError): string {
  return CONNECT_COPY[error.reason];
}

/**
 * What to call the computer a settings panel is describing.
 *
 * These panels configure the machine this window works on: this computer by
 * default, and another one while attached. Copy that says "this machine"
 * regardless names the wrong host, and a reader who believes it makes the
 * wrong call about where their files, their commands, and their audio go.
 */
export function hostMachineLabel(): string {
  return attachedRemotely() ? "the machine" : "this computer";
}

/** What each host capability is called where a reader can see it. */
const AUTHORITY_LABEL: Record<HostAuthority, string> = {
  folder_broker: "Connected folders",
  client_executor: "Tool calls that run on your computer",
  native_export: "Saving files to this computer",
  computer_use: "Computer use",
};

export function hostAuthorityLabel(authority: HostAuthority): string {
  return AUTHORITY_LABEL[authority];
}

/** Every capability, in the order the panel lists them. */
export const HOST_AUTHORITIES: HostAuthority[] = [
  "folder_broker",
  "client_executor",
  "native_export",
  "computer_use",
];

/**
 * What a refused host command means, said once for all four.
 *
 * The cause is the same in every case — this window is attached to another
 * machine — so the sentence names the cause, and the capability's own label
 * carries the rest. Use it wherever a host command's failure reaches a reader,
 * so a refusal never surfaces as its raw code.
 */
export function hostErrorMessage(error: unknown, fallback: string): string {
  const authority = hostAuthorityRefusal(error);
  if (authority) {
    return `${AUTHORITY_LABEL[authority]} is not available while this window is attached to a remote machine. It reaches this computer, and your conversation is not on it.`;
  }
  return friendlyErrorMessage(error, fallback);
}
