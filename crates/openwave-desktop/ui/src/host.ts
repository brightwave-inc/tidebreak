import { invoke, isTauri } from "@tauri-apps/api/core";
import type { Chat } from "./api";

export type ConnectedFolder = {
  rootId: string;
  displayName: string;
};

export function hasNativeHost(): boolean {
  return isTauri();
}

export function listConnectedFolders(chat: Chat): Promise<ConnectedFolder[]> {
  return invoke("list_connected_folders", { chatId: chat.id });
}

export function connectFolder(chat: Chat): Promise<ConnectedFolder | null> {
  return invoke("connect_folder", {
    request: { chatId: chat.id },
  });
}

export function disconnectFolder(chat: Chat, rootId: string): Promise<boolean> {
  return invoke("disconnect_folder", {
    request: { chatId: chat.id, rootId },
  });
}
