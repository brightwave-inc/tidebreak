import { useEffect, useState } from "react";
import type { Chat } from "./api";
import {
  connectApprovedFolder,
  connectFolder,
  disconnectFolder,
  listApprovedFolders,
  listConnectedFolders,
  type ConnectedFolder,
} from "./host";
import { ChatTabs } from "./ChatTabs";

/**
 * A chat's connected folders: the directories the native host may read on this
 * conversation's behalf. Chat-scoped like sources and outputs, so it lives
 * behind the same per-chat tab control rather than a global side panel.
 * Folders already approved on this device can be reused without choosing them
 * from the picker again.
 */
export function FoldersView({ chat }: { chat: Chat }) {
  const [folders, setFolders] = useState<ConnectedFolder[]>([]);
  const [approvedFolders, setApprovedFolders] = useState<ConnectedFolder[]>([]);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const connectedIds = new Set(folders.map((folder) => folder.rootId));
  const availableFolders = approvedFolders.filter(
    (folder) => !connectedIds.has(folder.rootId),
  );

  async function refresh() {
    setError(null);
    try {
      const [connected, approved] = await Promise.all([
        listConnectedFolders(chat),
        listApprovedFolders(),
      ]);
      setFolders(connected);
      setApprovedFolders(approved);
    } catch (err) {
      setError(String(err));
    }
  }

  useEffect(() => {
    void refresh();
  }, [chat.id, chat.project_id]);

  async function addFolder() {
    setWorking(true);
    setError(null);
    try {
      const connected = await connectFolder(chat);
      if (connected) await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  async function addApprovedFolder(rootId: string) {
    setWorking(true);
    setError(null);
    try {
      const connected = await connectApprovedFolder(chat, rootId);
      if (connected) await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  async function removeFolder(rootId: string) {
    setWorking(true);
    setError(null);
    try {
      await disconnectFolder(chat, rootId);
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  return (
    <section className="folders-view" aria-labelledby="folders-title">
      <header className="folders-header">
        <div>
          <p className="eyebrow">This conversation</p>
          <h1 id="folders-title">Folders</h1>
          <p>
            OpenWave can read only the folders attached to this chat. Folders
            you approved before can be reused without choosing them again.
          </p>
        </div>
        <div className="folders-header-actions">
          <ChatTabs />
          <button
            type="button"
            className="btn btn-primary"
            disabled={working}
            onClick={() => void addFolder()}
          >
            {working ? "Working…" : "Choose another folder…"}
          </button>
        </div>
      </header>

      <div className="folders-content">
        {error && (
          <div className="document-error" role="alert">
            {error}
          </div>
        )}

        <div className="folders-section">
          {folders.length > 0 && (
            <p className="folders-section-label">Connected</p>
          )}
          {folders.length === 0 && !error ? (
            <p className="folders-empty">
              No folders connected to this chat.
            </p>
          ) : (
            <div className="folders-list">
              {folders.map((folder) => (
                <div className="folder-row" key={folder.rootId}>
                  <div className="folder-meta">
                    <strong className="folder-name">
                      {folder.displayName}
                    </strong>
                    <span className="folder-access">read access</span>
                  </div>
                  <button
                    type="button"
                    className="btn"
                    disabled={working}
                    onClick={() => void removeFolder(folder.rootId)}
                  >
                    Disconnect
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>

        {availableFolders.length > 0 && (
          <div className="folders-section">
            <p className="folders-section-label">Available on this Mac</p>
            <div className="folders-list">
              {availableFolders.map((folder) => (
                <div className="folder-row" key={folder.rootId}>
                  <div className="folder-meta">
                    <strong className="folder-name">
                      {folder.displayName}
                    </strong>
                    <span className="folder-access">previously approved</span>
                  </div>
                  <button
                    type="button"
                    className="btn"
                    disabled={working}
                    onClick={() => void addApprovedFolder(folder.rootId)}
                  >
                    Connect
                  </button>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </section>
  );
}
