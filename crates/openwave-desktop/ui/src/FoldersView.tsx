import { FolderOpenIcon, PlusIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { Chat, ConsentStatementSnapshot } from "./api";
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
  listCapabilityConsents,
  listConnectedFolders,
  revokeCapabilityConsent,
  type ConnectedFolder,
} from "./host";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "./NativePickerLatch";
import {
  folderAccessLabel,
  folderReach,
  folderStatements,
  type FolderReach,
} from "./FolderAccess";
import { verbLabel } from "./settings/PermissionsPanel";
import { useRefreshSignals } from "./RefreshSignals";

/**
 * A chat's connected folders: the directories the native host may reach on
 * this conversation's behalf.
 *
 * This panel and the Permissions panel are two groupings of the same consent
 * statements — this one by folder, that one by what the statements reach.
 * Each folder row lists the statements that name it, with the same revocation
 * the Permissions panel offers, so what a folder allows can be narrowed here
 * without disconnecting it.
 */
export function FoldersView({ chat }: { chat: Chat }) {
  const [folders, setFolders] = useState<ConnectedFolder[]>([]);
  const [consents, setConsents] = useState<ConsentStatementSnapshot[]>([]);
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
      const [connected, approved, statements] = await Promise.all([
        listConnectedFolders(chat),
        listApprovedFolders(),
        listCapabilityConsents(),
      ]);
      if (generation !== refreshGeneration.current) return;
      setFolders(connected);
      setApprovedFolders(approved);
      setConsents(statements);
    } catch (err) {
      if (generation !== refreshGeneration.current) return;
      setError(String(err));
    }
  }

  useEffect(() => {
    setFolders([]);
    setApprovedFolders([]);
    setConsents([]);
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

  async function revokeStatement(
    folder: ConnectedFolder,
    statement: ConsentStatementSnapshot,
  ) {
    const capability =
      statement.verb.kind === "capability" ? statement.verb.capability : null;
    const accepted = await confirm({
      title: `Revoke “${verbLabel(statement.verb)}” for ${folder.displayName}?`,
      description:
        capability === "read_files"
          ? "The agent can no longer read this folder — and loses command access to it, which depends on reading."
          : `The agent loses “${verbLabel(statement.verb).toLowerCase()}” for this folder. It stays connected.`,
      confirmLabel: "Revoke",
      destructive: true,
    });
    if (!accepted) return;
    setWorking(true);
    setError(null);
    try {
      await revokeCapabilityConsent(statement);
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
                {folders.map((folder) => {
                  const statements = folderStatements(
                    consents,
                    folder.rootId,
                    chat,
                  );
                  return (
                    <FolderCard
                      key={folder.rootId}
                      name={folder.displayName}
                      reach={folderReach(statements)}
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
                    >
                      {statements.map((statement) => (
                        <div
                          key={
                            statement.handle.kind === "capability_grant"
                              ? statement.handle.grant_id
                              : statement.granted_at
                          }
                          className="flex items-center justify-between gap-3"
                        >
                          <span className="text-sm text-muted-foreground">
                            {verbLabel(statement.verb)}
                          </span>
                          <Button
                            variant="ghost"
                            size="xs"
                            disabled={working}
                            onClick={() =>
                              void revokeStatement(folder, statement)
                            }
                          >
                            Revoke
                          </Button>
                        </div>
                      ))}
                    </FolderCard>
                  );
                })}
              </FolderSection>
            )}

            {availableFolders.length > 0 && (
              <FolderSection label="Available on this Mac">
                {availableFolders.map((folder) => (
                  <FolderCard
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
 * The folder's access state, read off the consent statements the broker
 * reports rather than assumed from what the app requested — the moment a
 * statement is revoked, the badge follows.
 */
function AccessBadge({ reach }: { reach: readonly FolderReach[] }) {
  const label = folderAccessLabel(reach);
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

function FolderCard({
  name,
  reach,
  badge,
  action,
  children,
}: {
  name: string;
  reach?: readonly FolderReach[];
  badge?: React.ReactNode;
  action: React.ReactNode;
  children?: React.ReactNode;
}) {
  return (
    <Card className="flex flex-col gap-2 px-3 py-2.5">
      <div className="flex flex-row items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <FolderOpenIcon className="size-4 shrink-0 text-muted-foreground" />
          <span className="truncate text-sm font-medium">{name}</span>
          {reach ? <AccessBadge reach={reach} /> : badge}
        </div>
        <div className="shrink-0">{action}</div>
      </div>
      {children}
    </Card>
  );
}
