import { FolderOpenIcon, PlusIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { Chat } from "./api";
import { PanelSecondaryHeader } from "@/components/PanelHeader";
import { useConfirm } from "@/components/ConfirmDialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  connectApprovedFolder,
  connectFolder,
  disconnectFolder,
  listApprovedFolders,
  listConnectedFolders,
  type ConnectedFolder,
  type ConnectedFolderAccess,
  type FolderCapability,
} from "./host";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "./NativePickerLatch";
import { folderAccessLabel } from "./FolderAccess";
import { useRefreshSignals } from "./RefreshSignals";

/**
 * A chat's connected folders: the directories the native host may reach on this
 * conversation's behalf, each shown with the access the broker actually grants
 * it.
 *
 * Two sections rather than one. The first is what this conversation can reach.
 * The second is what this device has already approved and can be reattached
 * without going back through the picker — an affordance that only makes sense
 * for an app holding its own grants, and the reason this panel is not simply
 * a list.
 */
export function FoldersView({ chat }: { chat: Chat }) {
  const [folders, setFolders] = useState<ConnectedFolderAccess[]>([]);
  const [approvedFolders, setApprovedFolders] = useState<ConnectedFolder[]>([]);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refreshGeneration = useRef(0);
  const folderAccess = useRefreshSignals((state) => state.folderAccess);
  const { confirm, dialog: confirmDialog } = useConfirm();
  const connectedIds = new Set(folders.map((folder) => folder.rootId));
  const availableFolders = approvedFolders.filter(
    (folder) => !connectedIds.has(folder.rootId),
  );

  async function refresh() {
    const generation = ++refreshGeneration.current;
    setError(null);
    try {
      const [connected, approved] = await Promise.all([
        listConnectedFolders(chat),
        listApprovedFolders(),
      ]);
      if (generation !== refreshGeneration.current) return;
      setFolders(connected);
      setApprovedFolders(approved);
    } catch (err) {
      if (generation !== refreshGeneration.current) return;
      setError(String(err));
    }
  }

  useEffect(() => {
    setFolders([]);
    setApprovedFolders([]);
    setWorking(false);
    setError(null);
    void refresh();
    return () => {
      refreshGeneration.current += 1;
    };
  }, [chat.id, chat.project_id, folderAccess]);

  /**
   * Run one native picker interaction under the app-wide latch.
   *
   * Both of these open a host window, and the host allows one at a time. Taking
   * the latch is what turns a second attempt into a sentence a reader can act
   * on instead of the host's own rejection string.
   */
  async function withPicker(holder: string, open: () => Promise<unknown>) {
    if (!useNativePickerLatch.getState().claim(holder)) {
      setError(PICKER_BUSY_MESSAGE);
      return;
    }
    setWorking(true);
    setError(null);
    try {
      const connected = await open();
      if (connected) useRefreshSignals.getState().signal("folderAccess");
    } catch (err) {
      setError(String(err));
    } finally {
      useNativePickerLatch.getState().release(holder);
      setWorking(false);
    }
  }

  async function addFolder() {
    await withPicker(PICKER_HOLDERS.connectFolder, () => connectFolder(chat));
  }

  async function addApprovedFolder(rootId: string) {
    await withPicker(PICKER_HOLDERS.confirmApprovedFolder, () =>
      connectApprovedFolder(chat, rootId),
    );
  }

  async function removeFolder(folder: ConnectedFolder) {
    const accepted = await confirm({
      title: `Disconnect ${folder.displayName}?`,
      description: "The agent loses access to this folder.",
      confirmLabel: "Disconnect",
      destructive: true,
    });
    if (!accepted) return;
    setWorking(true);
    setError(null);
    try {
      await disconnectFolder(chat, folder.rootId);
      useRefreshSignals.getState().signal("folderAccess");
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  const connectButton = (
    <Button size="sm" disabled={working} onClick={() => void addFolder()}>
      <PlusIcon className="size-4" />
      {working ? "Working…" : "Connect folder"}
    </Button>
  );

  const nothingToShow = folders.length === 0 && availableFolders.length === 0;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PanelSecondaryHeader showBorder={false} className="pr-1 pl-4">
        <div className="flex items-baseline gap-3">
          <h1 className="text-lg font-medium">Folders</h1>
          {folders.length > 0 && (
            <span className="text-lg font-medium text-muted-foreground">
              {folders.length}
            </span>
          )}
        </div>
        <span className="grow" />
        <div className="pr-2">{connectButton}</div>
      </PanelSecondaryHeader>

      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-auto p-4">
        {error && (
          <div
            className="shrink-0 rounded-md bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted"
            role="alert"
          >
            {error}
          </div>
        )}

        {nothingToShow && !error ? (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <FolderOpenIcon />
              </EmptyMedia>
              <EmptyTitle>No folders connected</EmptyTitle>
              <EmptyDescription>
                Connect a folder to let OpenWave work with files on your
                computer in this conversation. It can reach only the folders you
                attach here, and each one shows what it allows.
              </EmptyDescription>
            </EmptyHeader>
            <EmptyContent>{connectButton}</EmptyContent>
          </Empty>
        ) : (
          <>
            {folders.length > 0 && (
              <FolderSection label="Connected">
                {folders.map((folder) => (
                  <FolderRow
                    key={folder.rootId}
                    name={folder.displayName}
                    badge={<AccessBadge capabilities={folder.capabilities} />}
                    action={
                      <Button
                        variant="outline"
                        size="xs"
                        disabled={working}
                        onClick={() => void removeFolder(folder)}
                      >
                        Disconnect
                      </Button>
                    }
                  />
                ))}
              </FolderSection>
            )}

            {availableFolders.length > 0 && (
              <FolderSection label="Available on this Mac">
                {availableFolders.map((folder) => (
                  <FolderRow
                    key={folder.rootId}
                    name={folder.displayName}
                    badge={
                      <Badge variant="outline" size="sm">
                        Previously approved
                      </Badge>
                    }
                    action={
                      <Button
                        variant="outline"
                        size="xs"
                        disabled={working}
                        onClick={() => void addApprovedFolder(folder.rootId)}
                      >
                        Connect
                      </Button>
                    }
                  />
                ))}
              </FolderSection>
            )}
          </>
        )}
      </div>
      {confirmDialog}
    </div>
  );
}

/**
 * The folder's access state, read off what the broker reports rather than
 * assumed from what the app requested.
 *
 * Connecting a folder currently grants reading and writing together, so this
 * renders "Read and write" in practice. It is derived anyway: the moment the
 * grant ladder narrows, a badge computed from a constant would keep claiming
 * access the agent no longer has, which is the failure worth designing out.
 */
function AccessBadge({ capabilities }: { capabilities: FolderCapability[] }) {
  const label = folderAccessLabel(capabilities);
  const hasAccess = label !== "No access";
  return (
    <Badge variant={hasAccess ? "secondary" : "outline"} size="sm">
      {label}
    </Badge>
  );
}

function FolderSection({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <section className="flex shrink-0 flex-col gap-2">
      <h2 className="text-xs font-medium text-muted-foreground">{label}</h2>
      <div className="flex flex-col gap-2">{children}</div>
    </section>
  );
}

function FolderRow({
  name,
  badge,
  action,
}: {
  name: string;
  badge: React.ReactNode;
  action: React.ReactNode;
}) {
  return (
    <Card className="flex flex-row items-center justify-between gap-3 px-3 py-2.5">
      <div className="flex min-w-0 items-center gap-2">
        <FolderOpenIcon className="size-4 shrink-0 text-muted-foreground" />
        <span className="truncate text-sm font-medium">{name}</span>
        {badge}
      </div>
      <div className="shrink-0">{action}</div>
    </Card>
  );
}
