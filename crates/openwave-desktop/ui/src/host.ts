import { invoke, isTauri } from "@tauri-apps/api/core";
import type { Chat } from "./api";

export type ConnectedFolder = {
  rootId: string;
  displayName: string;
};

export type FolderAccessDecision = "allow" | "decline";

export function hasNativeHost(): boolean {
  return isTauri();
}

export function listConnectedFolders(chat: Chat): Promise<ConnectedFolder[]> {
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
