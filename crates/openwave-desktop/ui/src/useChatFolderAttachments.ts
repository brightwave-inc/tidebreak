import { useEffect, useRef, useState } from "react";

import type { Chat } from "./api";
import {
  connectFolder,
  disconnectFolder,
  listConnectedFolders,
  type ConnectedFolderAccess,
} from "./host";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "./NativePickerLatch";
import { useRefreshSignals } from "./RefreshSignals";

export type ChatFolderAttachments = {
  items: ConnectedFolderAccess[];
  working: boolean;
  error: string | null;
  attach: () => void;
  remove: (rootId: string) => void;
};

/**
 * The compact folder controls beside the composer and the full Folders panel
 * share the same host reconciliation path. The refresh signal keeps both
 * surfaces honest when either one changes the chat's grants.
 */
export function useChatFolderAttachments(
  chat: Chat | null,
  nativeHost: boolean,
): ChatFolderAttachments {
  const [items, setItems] = useState<ConnectedFolderAccess[]>([]);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refreshGeneration = useRef(0);
  const folderAccess = useRefreshSignals((state) => state.folderAccess);

  useEffect(() => {
    const generation = ++refreshGeneration.current;
    if (!chat || !nativeHost) {
      setItems([]);
      setError(null);
      return;
    }
    setError(null);
    void listConnectedFolders(chat).then(
      (folders) => {
        if (generation === refreshGeneration.current) setItems(folders);
      },
      (reason) => {
        if (generation === refreshGeneration.current) setError(String(reason));
      },
    );
    return () => {
      refreshGeneration.current += 1;
    };
  }, [chat?.id, chat?.project_id, nativeHost, folderAccess]);

  async function attach() {
    if (!chat || !nativeHost || working) return;
    if (!useNativePickerLatch.getState().claim(PICKER_HOLDERS.connectFolder)) {
      setError(PICKER_BUSY_MESSAGE);
      return;
    }
    setWorking(true);
    setError(null);
    try {
      const connected = await connectFolder(chat);
      if (connected) {
        useRefreshSignals.getState().signal("folderAccess");
      }
    } catch (reason) {
      setError(String(reason));
    } finally {
      useNativePickerLatch.getState().release(PICKER_HOLDERS.connectFolder);
      setWorking(false);
    }
  }

  async function remove(rootId: string) {
    if (!chat || !nativeHost || working) return;
    setWorking(true);
    setError(null);
    try {
      await disconnectFolder(chat, rootId);
      useRefreshSignals.getState().signal("folderAccess");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  }

  return {
    items,
    working,
    error,
    attach: () => void attach(),
    remove: (rootId) => void remove(rootId),
  };
}
