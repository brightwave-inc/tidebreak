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
import { SettingsError, SettingsPanel } from "./settings/primitives";
import { Button } from "@/components/ui/button";

export function FoldersPanel({ chat }: { chat: Chat }) {
  const [folders, setFolders] = useState<ConnectedFolder[]>([]);
  const [approvedFolders, setApprovedFolders] = useState<ConnectedFolder[]>([]);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const scopeLabel = "chat";
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
    <SettingsPanel
      title="Connected folders"
      description={`OpenWave can read only folders attached to this ${scopeLabel}. Previously approved folders can be reused without choosing them again.`}
    >
      <Button
        type="button"
        variant="outline"
        className="self-start"
        disabled={working}
        onClick={() => void addFolder()}
      >
        Choose another folder…
      </Button>
      <div className="flex flex-col gap-2">
        {folders.length > 0 && (
          <p className="text-xs font-medium text-muted-foreground">Connected</p>
        )}
        {folders.length === 0 && !error && (
          <p className="text-sm text-muted-foreground">
            No folders connected to this {scopeLabel}.
          </p>
        )}
        {folders.map((folder) => (
          <div
            className="flex items-center justify-between gap-3 rounded-lg border border-border p-3"
            key={folder.rootId}
          >
            <div className="min-w-0">
              <strong className="block truncate text-sm font-medium">
                {folder.displayName}
              </strong>
              <span className="text-xs text-muted-foreground">read access</span>
            </div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={working}
              onClick={() => void removeFolder(folder.rootId)}
            >
              Disconnect
            </Button>
          </div>
        ))}
      </div>
      {availableFolders.length > 0 && (
        <div className="flex flex-col gap-2">
          <p className="text-xs font-medium text-muted-foreground">
            Available on this Mac
          </p>
          {availableFolders.map((folder) => (
            <div
              className="flex items-center justify-between gap-3 rounded-lg border border-border p-3"
              key={folder.rootId}
            >
              <div className="min-w-0">
                <strong className="block truncate text-sm font-medium">
                  {folder.displayName}
                </strong>
                <span className="text-xs text-muted-foreground">
                  previously approved
                </span>
              </div>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={working}
                onClick={() => void addApprovedFolder(folder.rootId)}
              >
                Connect
              </Button>
            </div>
          ))}
        </div>
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}
