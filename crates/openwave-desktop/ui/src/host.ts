import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Chat } from "./api";

export type ConnectedFolder = {
  rootId: string;
  displayName: string;
};

/** What the agent may do inside one connected folder. */
export type FolderCapability = "read" | "write";

/**
 * A connected folder and the access the host broker reports for it, checked
 * against the same authorization the agent's own operations go through. The app
 * must not substitute an assumption for this — a folder's access state is what
 * the broker would allow, not what the app asked for.
 */
export type ConnectedFolderAccess = ConnectedFolder & {
  capabilities: FolderCapability[];
};

export type FolderAccessDecision = "allow" | "decline";
export type OutputWritebackDecision = "allow" | "decline";

export function hasNativeHost(): boolean {
  return isTauri();
}

/** Best-effort only; durable pending-question polling remains authoritative. */
export async function requestUserAttention(): Promise<void> {
  if (!isTauri()) return;
  await invoke("request_user_attention");
}

export function listConnectedFolders(
  chat: Chat,
): Promise<ConnectedFolderAccess[]> {
  return invoke("list_connected_folders", { chatId: chat.id });
}

export function listApprovedFolders(): Promise<ConnectedFolder[]> {
  return invoke("list_approved_folders");
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

export function disconnectFolder(chat: Chat, rootId: string): Promise<boolean> {
  return invoke("disconnect_folder", {
    request: { chatId: chat.id, rootId },
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
 * Windows/Linux keep standard decorations; the browser has its own chrome.
 */
export function hasMacOverlayTitlebar(): boolean {
  return hasNativeHost() && navigator.userAgent.includes("Mac OS");
}

/**
 * Open a URL in the user's default browser.
 *
 * The webview swallows `window.open` and `target="_blank"` (no new-window
 * handler by design), so anything that must leave the app — the gateway
 * sign-in page — goes through the shell plugin, whose `open` permission
 * validates the URL scheme. Returns false outside the native host so callers
 * can fall back to `window.open` in a plain browser.
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
 * `crates/openwave-desktop/src/deep_link.rs`.
 */
export function onPairingChanged(handler: () => void): () => void {
  if (!isTauri()) return () => {};
  const subscription = listen("gateway:pairing-changed", () => handler());
  return () => {
    void subscription.then((unlisten) => unlisten());
  };
}
