import { useEffect, useState } from "react";
import type { Chat } from "./api";
import {
  connectFolder,
  disconnectFolder,
  listConnectedFolders,
  type ConnectedFolder,
} from "./host";
import { SettingsError, SettingsPanel } from "./settings/primitives";
import { Button } from "@/components/ui/button";

export function FoldersPanel({ chat }: { chat: Chat }) {
  const [folders, setFolders] = useState<ConnectedFolder[]>([]);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const scopeLabel = "chat";

  async function refresh() {
    setError(null);
    try {
      setFolders(await listConnectedFolders(chat));
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
      description={`OpenWave can read only folders you choose for this ${scopeLabel}. Folder locations stay with the native host.`}
    >
      <Button
        type="button"
        variant="outline"
        className="self-start"
        disabled={working}
        onClick={() => void addFolder()}
      >
        Choose folder…
      </Button>
      <div className="flex flex-col gap-2">
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
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}
