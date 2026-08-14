import { useEffect, useRef, useState } from "react";

import type { Chat } from "./api";
import {
  connectApprovedFolder,
  connectFolder,
  disconnectFolder,
  listApprovedFolders,
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
  /**
   * Every folder approved on this device, attached here or not. The composer's
   * `@` list draws the unattached ones from it: an approval outlives the chat
   * it was granted in, so a second conversation can reach the folder by name
   * instead of finding it on disk again.
   */
  approved: ConnectedFolder[];
  working: boolean;
  error: string | null;
  attach: () => Promise<ConnectedFolder | null>;
  connectApproved: (rootId: string) => Promise<ConnectedFolder | null>;
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
  const [approved, setApproved] = useState<ConnectedFolder[]>([]);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refreshGeneration = useRef(0);
  const folderAccess = useRefreshSignals((state) => state.folderAccess);

  useEffect(() => {
    const generation = ++refreshGeneration.current;
    if (!chat || !nativeHost) {
      setItems([]);
      setApproved([]);
      setError(null);
      return;
    }
    setError(null);
    void Promise.all([
      listConnectedFolders(chat),
      listCapabilityConsents(),
      listApprovedFolders(),
    ]).then(
      ([folders, consents, approvedFolders]) => {
        if (generation !== refreshGeneration.current) return;
        setApproved(approvedFolders);
        setItems(
          folders
            // The composer chips are working controls for the current turn;
            // an unavailable folder cannot serve it. The full Folders panel
            // is where the set-aside state is shown and acted on.
            .filter((folder) => folder.status === "connected")
            .map((folder) => ({
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

  async function attach(): Promise<ConnectedFolder | null> {
    if (!chat || !nativeHost || working) return null;
    if (!useNativePickerLatch.getState().claim(PICKER_HOLDERS.connectFolder)) {
      setError(PICKER_BUSY_MESSAGE);
      return null;
    }
    setWorking(true);
    setError(null);
    try {
      const connected = await connectFolder(chat);
      if (connected) {
        useRefreshSignals.getState().signal("folderAccess");
      }
      return connected;
    } catch (reason) {
      setError(String(reason));
      return null;
    } finally {
      useNativePickerLatch.getState().release(PICKER_HOLDERS.connectFolder);
      setWorking(false);
    }
  }

  /**
   * Attach a folder this device already approved.
   *
   * No picker: the host confirms the re-attachment natively and the broker
   * re-checks the approval, so the renderer never names a place on disk. The
   * grant is the host's to make either way — this only spares the reader
   * finding a folder they have already chosen once.
   */
  async function connectApproved(
    rootId: string,
  ): Promise<ConnectedFolder | null> {
    if (!chat || !nativeHost || working) return null;
    setWorking(true);
    setError(null);
    try {
      const connected = await connectApprovedFolder(chat, rootId);
      if (connected) {
        useRefreshSignals.getState().signal("folderAccess");
      }
      return connected;
    } catch (reason) {
      setError(String(reason));
      return null;
    } finally {
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
    approved,
    working,
    error,
    attach,
    connectApproved,
    remove: (rootId) => void remove(rootId),
  };
}
