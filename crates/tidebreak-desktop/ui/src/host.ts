import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Chat, ConsentStatementSnapshot } from "./api";

/** Whether the broker can currently reach a listed folder. "unavailable" is
 * the set-aside state: the approval and attachment stand, but the directory
 * could not be reopened — an unplugged drive, a moved folder. */
export type FolderStatus = "connected" | "unavailable";

export type ConnectedFolder = {
  rootId: string;
  displayName: string;
  status: FolderStatus;
  availableInFutureChats: boolean;
};

export type FolderAccessDecision = "allow" | "decline";
export type OutputWritebackDecision = "allow" | "decline";

/**
 * A host capability that exists only on the machine this app runs on.
 *
 * While this client is attached to a remote machine, none of the four apply to
 * the conversation you are looking at, and each command that needs one refuses
 * rather than acting against the wrong host.
 */
export type HostAuthority =
  | "folder_broker"
  | "client_executor"
  | "native_export"
  | "computer_use";

/**
 * The stable reason each authority gives when it is unavailable, following the
 * precedent `output_writeback_authority_unavailable` set. These strings are the
 * contract with the native shell; the copy that reaches the user is the
 * renderer's, derived from the authority rather than from the code.
 */
const AUTHORITY_BY_REASON: Record<string, HostAuthority> = {
  folder_broker_authority_unavailable: "folder_broker",
  client_executor_authority_unavailable: "client_executor",
  native_export_authority_unavailable: "native_export",
  computer_use_authority_unavailable: "computer_use",
};

/**
 * Which authority a failed host command refused for, or `null` if it failed for
 * some other reason.
 *
 * A refusal arrives as the bare reason code and nothing else, which is what
 * separates it from the free-text errors these commands otherwise return.
 */
export function hostAuthorityRefusal(error: unknown): HostAuthority | null {
  const reason = typeof error === "string" ? error : null;
  if (reason === null) return null;
  return AUTHORITY_BY_REASON[reason] ?? null;
}

export function hasNativeHost(): boolean {
  return isTauri();
}

/**
 * Whether this window works on a machine other than this computer.
 *
 * The module-level twin of `useAttachedRemotely`, for the three callers that
 * sit outside React and cannot use a hook: the API client, the image
 * attachment hook's non-component branch, and the code-mode browser host.
 * Boot resolves the attachment before any of them runs and records it here.
 *
 * Defaults to `false`, which is what a browser tab is and what the desktop is
 * until told otherwise. The default has to be the permissive one: attachment
 * changes always reload the window, so a stale `true` could never be cleared,
 * while a stale `false` cannot outlive boot.
 */
let attachedRemotelyFlag = false;

export function setAttachedRemotely(attached: boolean): void {
  attachedRemotelyFlag = attached;
}

export function attachedRemotely(): boolean {
  return attachedRemotelyFlag;
}

/**
 * Whether host authority applies to what this window is working on.
 *
 * Prefer this to a bare {@link hasNativeHost} anywhere the call reaches this
 * computer's files, screen, or input. `hasNativeHost` alone stays true while
 * attached to another machine, so it offers controls that then refuse.
 */
export function hasLocalHostAuthority(): boolean {
  return hasNativeHost() && !attachedRemotely();
}

/** Native directory picker for code-mode repo registration and clone destinations. */
export async function pickCodeDirectory(): Promise<string | null> {
  if (!isTauri()) return null;
  const path = await invoke<string | null>("pick_code_directory");
  return path ?? null;
}

/** Best-effort only; durable pending-question polling remains authoritative. */
export async function requestUserAttention(): Promise<void> {
  if (!isTauri()) return;
  await invoke("request_user_attention");
}

/**
 * The folders this conversation has attached, by their safe identities. What
 * each folder *allows* is not part of this answer: access is rendered from
 * the same consent statements the Permissions surface shows
 * ([`listCapabilityConsents`]), so both panels are groupings of one model. A
 * folder whose access was revoked down to nothing is still listed — it is
 * still attached, and hiding it would hide the controls that undo that.
 */
export function listConnectedFolders(chat: {
  id: string;
}): Promise<ConnectedFolder[]> {
  return invoke("list_connected_folders", { chatId: chat.id });
}

export function listApprovedFolders(): Promise<ConnectedFolder[]> {
  return invoke("list_approved_folders");
}

/**
 * The capability half of the unified consent read model: every host-broker
 * grant over connected folders, in the same statement shape the server serves
 * for standing tool grants. Empty outside the native host — a browser build
 * has no broker and therefore no capability consent to report.
 */
export function listCapabilityConsents(): Promise<ConsentStatementSnapshot[]> {
  if (!isTauri()) return Promise.resolve([]);
  return invoke("list_capability_consents");
}

/**
 * Withdraw one host-broker capability grant by the statement that names it.
 * Returns whether anything was revoked; `false` means the grant was already
 * gone. Resolving `false` outside the native host keeps a stray browser call
 * from claiming a revocation nothing performed.
 */
export function revokeCapabilityConsent(
  statement: ConsentStatementSnapshot,
): Promise<boolean> {
  if (!isTauri() || statement.handle.kind !== "capability_grant") {
    return Promise.resolve(false);
  }
  return invoke("revoke_capability_consent", {
    request: {
      grantId: statement.handle.grant_id,
      level: statement.level,
    },
  });
}

export function connectFolder(chat: Chat): Promise<ConnectedFolder | null> {
  return invoke("connect_folder", {
    request: { chatId: chat.id },
  });
}

export function connectApprovedFolder(
  chat: Chat,
  rootId: string,
): Promise<ConnectedFolder | null> {
  return invoke("connect_approved_folder", {
    request: { chatId: chat.id, rootId },
  });
}

/** Attach every saved folder before a newly created chat can start work. */
export function attachTrustedFolders(chat: {
  id: string;
}): Promise<ConnectedFolder[]> {
  if (!hasLocalHostAuthority()) return Promise.resolve([]);
  return invoke("attach_trusted_folders", { chatId: chat.id });
}

/** Change whether one approved folder attaches to future chats. */
export function setTrustedFolder(
  rootId: string,
  trusted: boolean,
): Promise<boolean> {
  return invoke("set_trusted_folder", {
    request: { rootId, trusted },
  });
}

export function disconnectFolder(chat: Chat, rootId: string): Promise<boolean> {
  return invoke("disconnect_folder", {
    request: { chatId: chat.id, rootId },
  });
}

/**
 * Withdraw one folder's host approval, grants, and chat attachments everywhere.
 */
export function forgetFolder(rootId: string): Promise<boolean> {
  return invoke("forget_folder", {
    request: { rootId },
  });
}

/**
 * Forget host-broker authority for a conversation that has already been
 * deleted. Chat ids are never reused, so residual grants/attachments for that
 * subject are leftover authority with no product surface left. No-op outside
 * the native host.
 */
export function purgeDeletedConversationSubject(
  chatId: string,
): Promise<boolean> {
  if (!isTauri()) return Promise.resolve(false);
  return invoke("purge_deleted_conversation_subject", {
    request: { chatId },
  });
}

/** The reach a permissions surface can add to an already-attached folder.
 * Read is included: revoking it leaves the folder attached and allowing
 * nothing, so asking for it back has to be possible without disconnecting. */
export type WidenedFolderCapability =
  | "read_files"
  | "write_files"
  | "execute_commands";

/**
 * Ask to add one capability to a folder this chat already has attached. The
 * consent ceremony is native — a host dialog naming the chat, the folder, and
 * exactly what is being allowed — and the approval is recorded by the broker
 * as a fresh permission-dialog grant. Resolves `null` when the user cancels.
 */
export function grantFolderCapability(
  chat: { id: string },
  rootId: string,
  capability: WidenedFolderCapability,
): Promise<boolean | null> {
  return invoke("grant_folder_capability", {
    request: { chatId: chat.id, rootId, capability },
  });
}

export function resolveFolderAccessRequest(
  chatId: string,
  callId: string,
  decision: FolderAccessDecision,
): Promise<void> {
  return invoke("resolve_folder_access_request", {
    request: { chatId, callId, decision },
  });
}

export function resolveOutputWritebackRequest(
  chatId: string,
  callId: string,
  decision: OutputWritebackDecision,
): Promise<void> {
  return invoke("resolve_output_writeback_request", {
    request: { chatId, callId, decision },
  });
}

/**
 * The native macOS window uses an overlay titlebar (traffic lights over app
 * chrome), so the app renders its own drag strip with controls beside them.
 * Windows/Linux also render the app titlebar for history controls, but do not
 * need the traffic-light inset this predicate describes.
 */
export function hasMacOverlayTitlebar(): boolean {
  return hasNativeHost() && navigator.userAgent.includes("Mac OS");
}

/**
 * Open a URL in the user's default browser.
 *
 * The webview swallows `window.open` and `target="_blank"` (no new-window
 * handler by design), so anything that must leave the app — the gateway
 * sign-in page — goes through the shell plugin, whose `open` scope
 * (`plugins.shell.open` in `tauri.conf.json`) admits only `http(s)` URLs.
 * Returns false outside the native host so callers can fall back to
 * `window.open` in a plain browser.
 */
export async function openExternal(url: string): Promise<boolean> {
  if (!isTauri()) return false;
  await invoke("plugin:shell|open", { path: url });
  return true;
}

/**
 * Subscribe to the shell's nudge that a pending gateway pairing was parked or
 * replaced (a provision link, or a re-pair the user confirmed in the native
 * dialog). The sign-in gate refetches policy on it instead of waiting out a
 * poll tick. Returns an unsubscribe; no-op outside the native host, where no
 * deep link can land. The emitter is `PAIRING_CHANGED_EVENT` in
 * `crates/tidebreak-desktop/src/deep_link.rs`.
 */
export function onPairingChanged(handler: () => void): () => void {
  if (!isTauri()) return () => {};
  const subscription = listen("gateway:pairing-changed", () => handler());
  return () => {
    void subscription.then((unlisten) => unlisten());
  };
}

/**
 * Render this chat's diagnostic bundle for the clipboard. Built natively from
 * the event journal, not from what is on screen, and bounded so a chat with a
 * huge tool result cannot stall the clipboard write. See
 * `crates/tidebreak-desktop/src/chat_debug.rs`.
 */
export function copyChatDebugBundle(chatId: string): Promise<string> {
  return invoke("copy_chat_debug_bundle", { request: { chatId } });
}

/**
 * Write the complete, untruncated bundle to a file the reader picks. Resolves
 * `false` when the save dialog was dismissed.
 */
export function saveChatDebugBundle(chatId: string): Promise<boolean> {
  return invoke("save_chat_debug_bundle", { request: { chatId } });
}
