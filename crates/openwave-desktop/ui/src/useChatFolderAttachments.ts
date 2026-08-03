import { useEffect, useRef, useState } from "react";

import type { Chat } from "./api";
import {
  connectFolder,
  disconnectFolder,
  listCapabilityConsents,
  listConnectedFolders,
  type ConnectedFolder,
} from "./host";
import { folderStatements } from "./FolderAccess";
import type { ConsentStatementSnapshot } from "./api";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "./NativePickerLatch";
import { useRefreshSignals } from "./RefreshSignals";

/**
 * A connected folder joined with the consent statements that say what it
 * allows for this chat — the same rows the Permissions surface lists, so a
 * folder's access state cannot drift from what the broker holds.
 */
export type ChatFolderAccess = ConnectedFolder & {
  statements: ConsentStatementSnapshot[];
};

export type ChatFolderAttachments = {
  items: ChatFolderAccess[];
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
  const [items, setItems] = useState<ChatFolderAccess[]>([]);
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
    void Promise.all([listConnectedFolders(chat), listCapabilityConsents()]).then(
      ([folders, consents]) => {
        if (generation !== refreshGeneration.current) return;
        setItems(
          folders.map((folder) => ({
            ...folder,
            statements: folderStatements(consents, folder.rootId, chat),
          })),
        );
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
