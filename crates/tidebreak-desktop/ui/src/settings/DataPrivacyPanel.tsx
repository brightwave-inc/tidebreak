import { useCallback, useEffect, useState, type ReactNode } from "react";
import { toast } from "sonner";

import type {
  ApiClient,
  ConversationExportFormat,
  ConversationExportRequest,
  DataOverview,
} from "@/api";
import { useConfirm } from "@/components/ConfirmDialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { SegmentedControl } from "@/components/ui/segmented";
import { formatBytes } from "@/lib/formatBytes";
import { friendlyErrorMessage } from "@/lib/utils";
import {
  DELETE_ALL_DATA_PHRASE,
  type SavedFile,
  deleteAllData,
  hasLocalHostAuthority,
  revealDataDirectory,
  saveConversationExport,
  saveProfileBackup,
} from "@/host";
import {
  DATA_CATEGORY_COPY,
  OUTBOUND_TRAFFIC,
  RESET_SETTINGS_SCOPE,
  clearLocalPreferences,
  revealLabel,
} from "./dataPrivacy";
import {
  SettingsError,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

/** The native commands this page drives, injectable for stories and tests. */
export type DataPrivacyHost = {
  /** Whether the commands below reach this computer. */
  local: boolean;
  reveal: () => Promise<void>;
  saveBackup: () => Promise<SavedFile | null>;
  saveExport: (request: ConversationExportRequest) => Promise<SavedFile | null>;
  deleteAllData: (confirmation: string) => Promise<void>;
};

export function nativeDataPrivacyHost(): DataPrivacyHost {
  return {
    local: hasLocalHostAuthority(),
    reveal: revealDataDirectory,
    saveBackup: saveProfileBackup,
    saveExport: saveConversationExport,
    deleteAllData,
  };
}

/** A conversation the export can name. */
export type ExportableConversation = {
  id: string;
  title: string | null;
  created_at: string;
};

type DataClient = Pick<
  ApiClient,
  | "getDataOverview"
  | "resetSettings"
  | "downloadProfileBackup"
  | "downloadConversationExport"
>;

type Working = "backup" | "export" | "reset" | "delete" | null;

/**
 * Settings → Data and privacy: where the profile lives and how much disk it
 * uses, a backup and a conversation export, the list of what leaves this
 * computer, and the two actions that cannot be undone.
 *
 * The backup and the export are the server's (`/data`, which the CLI reaches
 * too). On the desktop they stream to a file the person picks in the native
 * save dialog; in a browser the file downloads. Deleting all data is the
 * desktop's alone, because it removes keychain items and quits the app.
 */
export function DataPrivacyPanel({
  client,
  host,
  attachedRemotely,
  conversations,
  onOpenSection,
  onReload = () => window.location.reload(),
  userAgent = typeof navigator === "undefined" ? "" : navigator.userAgent,
}: {
  client: DataClient;
  host: DataPrivacyHost;
  /** This window works on another machine, whose data is not this computer's. */
  attachedRemotely: boolean;
  conversations: readonly ExportableConversation[];
  /** Bring another settings section forward, by its path. */
  onOpenSection?: (path: string) => void;
  /** Reload the window, so reset local preferences take effect. */
  onReload?: () => void;
  userAgent?: string;
}) {
  const [overview, setOverview] = useState<DataOverview | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [working, setWorking] = useState<Working>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [exportOpen, setExportOpen] = useState(false);
  const { confirm, dialog: confirmDialog } = useConfirm();
  // What reaches this computer: native commands, and only while this window
  // works on it. A browser tab downloads instead and has nothing to delete.
  const local = host.local && !attachedRemotely;
  const browser = !host.local && !attachedRemotely;

  const reload = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      setOverview(await client.getDataOverview());
    } catch (caught) {
      setLoadError(
        friendlyErrorMessage(caught, "Could not read the data folder."),
      );
    } finally {
      setLoading(false);
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function run(kind: Working, work: () => Promise<void>) {
    setWorking(kind);
    setActionError(null);
    try {
      await work();
    } catch (caught) {
      setActionError(friendlyErrorMessage(caught, "Something went wrong."));
    } finally {
      setWorking(null);
    }
  }

  function reveal() {
    void host.reveal().catch((caught) => {
      toast.error(
        friendlyErrorMessage(caught, "Could not open the data folder."),
      );
    });
  }

  function backUp() {
    void run("backup", async () => {
      if (local) {
        const saved = await host.saveBackup();
        if (saved) {
          toast.success(`Backed up ${formatBytes(saved.bytes)}`, {
            description: saved.path,
          });
        }
        return;
      }
      const file = await client.downloadProfileBackup();
      downloadBlob(file.blob, file.fileName);
    });
  }

  function exportConversations(request: ConversationExportRequest) {
    void run("export", async () => {
      if (local) {
        const saved = await host.saveExport(request);
        if (!saved) return;
        setExportOpen(false);
        toast.success(exportedLabel(saved.count), { description: saved.path });
        return;
      }
      const file = await client.downloadConversationExport(request);
      setExportOpen(false);
      downloadBlob(file.blob, file.fileName);
    });
  }

  async function resetSettings() {
    const ok = await confirm({
      title: "Reset settings?",
      description: <ResetScope />,
      confirmLabel: "Reset settings",
      destructive: true,
    });
    if (!ok) return;
    void run("reset", async () => {
      await client.resetSettings();
      clearLocalPreferences(safeStorage());
      toast.success("Settings are back to their defaults");
      onReload();
    });
  }

  async function deleteEverything() {
    const ok = await confirm({
      title: "Delete all data?",
      description:
        "Tidebreak deletes every conversation, memory, setting, attachment, output, log, and backup on this computer, and removes your keys from the keychain. Then it quits. Code worktrees and your repositories stay. This cannot be undone.",
      confirmLabel: "Delete all data",
      destructive: true,
      requireText: DELETE_ALL_DATA_PHRASE,
    });
    if (!ok) return;
    void run("delete", async () => {
      // This window's own preferences live outside the data folder.
      clearAllStorage();
      await host.deleteAllData(DELETE_ALL_DATA_PHRASE);
    });
  }

  const busy = loading || working !== null;
  const reasonNoBackup = overview?.backup_unavailable ?? null;

  return (
    <SettingsPanel
      title="Data and privacy"
      description="Where Tidebreak keeps your data, how to back it up or take it with you, and what leaves this computer."
      busy={busy}
    >
      {attachedRemotely && (
        <SettingsStatus
          tone="neutral"
          label="This window works on another machine"
          description="The folder and disk use below are that machine's. Back up, export, or delete its data on the machine itself."
        />
      )}

      <SettingsSection
        title="Where your data lives"
        description="One folder holds everything but your keys, which stay in the keychain, and code worktrees, which stay in their own folder."
      >
        {loading && overview === null ? (
          <p className="text-sm text-muted-foreground" role="status">
            Reading the data folder…
          </p>
        ) : overview === null ? (
          <div className="flex flex-col items-start gap-3">
            <SettingsError>{loadError}</SettingsError>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => void reload()}
            >
              Try again
            </Button>
          </div>
        ) : (
          <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
            <div className="flex min-w-0 flex-1 basis-64 flex-col gap-1">
              <p className="text-sm font-medium">Data folder</p>
              <p className="font-mono text-xs break-all text-muted-foreground">
                {overview.data_dir}
              </p>
            </div>
            {local && (
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={reveal}
              >
                {revealLabel(userAgent)}
              </Button>
            )}
          </div>
        )}
      </SettingsSection>

      {overview && (
        <SettingsSection
          title="Disk use"
          description={`${formatBytes(overview.total_bytes)} in all.`}
        >
          <ul
            className="flex flex-col divide-y divide-border"
            aria-label="Disk use by category"
          >
            {overview.usage.map((entry) => (
              <li
                key={entry.category}
                className="flex items-baseline justify-between gap-4 py-2 first:pt-0 last:pb-0"
              >
                <span className="min-w-0">
                  <span className="text-sm">
                    {DATA_CATEGORY_COPY[entry.category].label}
                  </span>
                  <span className="block text-xs text-muted-foreground">
                    {DATA_CATEGORY_COPY[entry.category].description}
                  </span>
                </span>
                <span className="shrink-0 text-sm tabular-nums">
                  {formatBytes(entry.bytes)}
                </span>
              </li>
            ))}
          </ul>
        </SettingsSection>
      )}

      {(local || browser) && (
        <SettingsSection
          title="Back up and export"
          description="Take a copy with you, or keep one somewhere safe."
        >
          <ActionRow
            title="Back up"
            description={
              reasonNoBackup ??
              "The database with your conversations and memory, the files you attached, and the files Tidebreak made, as one .tar.gz. Your keys are not in it."
            }
          >
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={
                working !== null || overview === null || reasonNoBackup !== null
              }
              onClick={backUp}
            >
              {working === "backup" ? "Backing up…" : "Back up…"}
            </Button>
          </ActionRow>
          <ActionRow
            title="Export conversations"
            description="Your conversations as Markdown files or one JSON file, all of them or the ones you choose."
          >
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={working !== null || conversations.length === 0}
              onClick={() => setExportOpen(true)}
            >
              Export…
            </Button>
          </ActionRow>
        </SettingsSection>
      )}

      <SettingsSection
        title="What leaves this computer"
        description="Tidebreak sends data only for these, and only when the feature is in use."
      >
        <ul
          className="flex flex-col divide-y divide-border"
          aria-label="What leaves this computer"
        >
          {OUTBOUND_TRAFFIC.map((item) => (
            <li
              key={item.id}
              className="flex flex-col gap-1 py-3 first:pt-0 last:pb-0"
            >
              <p className="text-sm font-medium">{item.title}</p>
              <p className="text-sm text-muted-foreground">{item.what}</p>
              <p className="text-xs text-muted-foreground">
                <span className="font-medium text-foreground">
                  To turn it off:
                </span>{" "}
                {item.off}
                {item.section && onOpenSection && (
                  <>
                    {" "}
                    <button
                      type="button"
                      className="underline underline-offset-2 hover:text-foreground"
                      onClick={() => onOpenSection(item.section?.path ?? "")}
                    >
                      Open {item.section.label}
                    </button>
                  </>
                )}
              </p>
            </li>
          ))}
        </ul>
      </SettingsSection>

      {!attachedRemotely && (
        <SettingsSection title="Danger zone">
          <ActionRow
            title="Reset settings"
            description="Put preferences back to their defaults. Conversations, memory, and keys stay."
          >
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={working !== null}
              onClick={() => void resetSettings()}
            >
              {working === "reset" ? "Resetting…" : "Reset settings…"}
            </Button>
          </ActionRow>
          {local && (
            <ActionRow
              title="Delete all data"
              description="Delete every conversation, memory, setting, and file on this computer, and your keys. Tidebreak quits when it is done."
            >
              <Button
                type="button"
                variant="destructive"
                size="sm"
                disabled={working !== null}
                onClick={() => void deleteEverything()}
              >
                {working === "delete" ? "Deleting…" : "Delete all data…"}
              </Button>
            </ActionRow>
          )}
        </SettingsSection>
      )}

      {actionError && <SettingsError>{actionError}</SettingsError>}
      <ExportDialog
        open={exportOpen}
        conversations={conversations}
        exporting={working === "export"}
        onOpenChange={setExportOpen}
        onExport={exportConversations}
      />
      {confirmDialog}
    </SettingsPanel>
  );
}

function ActionRow({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
      <div className="min-w-0 flex-1 basis-64">
        <p className="text-sm font-medium">{title}</p>
        <p className="text-xs text-muted-foreground">{description}</p>
      </div>
      {children}
    </div>
  );
}

/** Exactly what Reset settings changes, and what it leaves alone. */
function ResetScope() {
  return (
    <div className="flex flex-col gap-2">
      <p>These go back to their defaults:</p>
      <ul className="list-disc space-y-1 pl-5">
        {RESET_SETTINGS_SCOPE.resets.map((line) => (
          <li key={line}>{line}</li>
        ))}
      </ul>
      <p>{RESET_SETTINGS_SCOPE.keeps}</p>
    </div>
  );
}

type ExportScope = "all" | "chosen";

function ExportDialog({
  open,
  conversations,
  exporting,
  onOpenChange,
  onExport,
}: {
  open: boolean;
  conversations: readonly ExportableConversation[];
  exporting: boolean;
  onOpenChange: (open: boolean) => void;
  onExport: (request: ConversationExportRequest) => void;
}) {
  const [format, setFormat] = useState<ConversationExportFormat>("markdown");
  const [scope, setScope] = useState<ExportScope>("all");
  const [chosen, setChosen] = useState<ReadonlySet<string>>(new Set());

  useEffect(() => {
    if (!open) return;
    setScope("all");
    setChosen(new Set());
  }, [open]);

  const ready = scope === "all" ? conversations.length > 0 : chosen.size > 0;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Export conversations</DialogTitle>
          <DialogDescription>
            Markdown writes one file per conversation in a .zip. JSON writes one
            file you can read with other tools.
          </DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-4">
          <SegmentedControl<ConversationExportFormat>
            aria-label="Export format"
            value={format}
            onValueChange={setFormat}
            options={[
              { value: "markdown", label: "Markdown" },
              { value: "json", label: "JSON" },
            ]}
          />
          <SegmentedControl<ExportScope>
            aria-label="Conversations to export"
            value={scope}
            onValueChange={setScope}
            options={[
              { value: "all", label: `All ${conversations.length}` },
              { value: "chosen", label: "Choose" },
            ]}
          />
          {scope === "chosen" && (
            <ul
              className="flex max-h-[min(40vh,20rem)] flex-col overflow-y-auto rounded-lg border"
              aria-label="Conversations"
            >
              {conversations.map((conversation) => {
                const checked = chosen.has(conversation.id);
                const label =
                  conversation.title?.trim() || "Untitled conversation";
                return (
                  <li
                    key={conversation.id}
                    className="border-b last:border-b-0"
                  >
                    <label className="flex cursor-pointer items-center gap-3 px-3 py-2">
                      <Checkbox
                        checked={checked}
                        aria-label={label}
                        onCheckedChange={(next) =>
                          setChosen((current) => {
                            const updated = new Set(current);
                            if (next === true) updated.add(conversation.id);
                            else updated.delete(conversation.id);
                            return updated;
                          })
                        }
                      />
                      <span className="min-w-0 flex-1 truncate text-sm">
                        {label}
                      </span>
                      <span className="shrink-0 text-xs text-muted-foreground">
                        {formatDay(conversation.created_at)}
                      </span>
                    </label>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            Cancel
          </Button>
          <Button
            type="button"
            disabled={!ready || exporting}
            onClick={() =>
              onExport(
                scope === "all"
                  ? { format }
                  : { format, chat_ids: [...chosen] },
              )
            }
          >
            {exporting
              ? "Exporting…"
              : scope === "chosen" && chosen.size > 0
                ? `Export ${chosen.size}`
                : "Export"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function exportedLabel(count: number | null): string {
  if (count === null) return "Exported your conversations";
  return `Exported ${count} ${count === 1 ? "conversation" : "conversations"}`;
}

function formatDay(timestamp: string): string {
  const date = new Date(timestamp);
  return Number.isNaN(date.getTime()) ? "" : date.toLocaleDateString();
}

function safeStorage(): Storage | undefined {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}

function clearAllStorage() {
  for (const read of [() => window.localStorage, () => window.sessionStorage]) {
    try {
      read().clear();
    } catch {
      // Blocked storage holds nothing to clear.
    }
  }
}

/** Save a file the server answered with, in a browser with no save dialog. */
function downloadBlob(blob: Blob, fileName: string) {
  const url = URL.createObjectURL(blob);
  const link = window.document.createElement("a");
  link.href = url;
  link.download = fileName;
  link.click();
  URL.revokeObjectURL(url);
}
