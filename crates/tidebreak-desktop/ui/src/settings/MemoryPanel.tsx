import { useCallback, useEffect, useState } from "react";
import { Brain, ChevronRight } from "lucide-react";
import { toast } from "sonner";

import type {
  ApiClient,
  MemoryDigest,
  MemoryKind,
  MemorySettings,
  MemoryRecord,
} from "../api";
import { Button } from "@/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { useConfirm } from "@/components/ConfirmDialog";
import { friendlyErrorMessage } from "@/lib/utils";
import { useMemoryPresenceStore } from "../MemoryPresenceStore";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

/** Where a record shows on the page. Preferences describe the person; the rest are notes. */
function groupOf(kind: MemoryKind): "about" | "notes" {
  return kind === "preference" ? "about" : "notes";
}

/** Past this share of the cap, the page says memory is nearly full. */
const NEARLY_FULL = 0.8;

type Draft = { id: string; title: string; body: string };

/**
 * Memory settings: one switch and what Tidebreak knows.
 *
 * Memory is automatic (decision 0099): the model saves as it goes and the
 * person's control is after the fact. So this page is a plain list of what
 * is known, in two groups, each line editable and forgettable with a way
 * back to the conversation it came from. No lifecycle states, revisions,
 * or meters: those are how the store works, not what the person needs.
 *
 * Forget stops Tidebreak using a record and keeps it, so a forgotten topic
 * is not learned again right away (decision 0099). Delete removes the record
 * and its history for good. Forgotten records sit in a collapsed list where
 * each one can come back or go for good.
 */
export function MemoryPanel({
  client,
  onOpenModels,
  onOpenConversation,
}: {
  client: ApiClient;
  /** Bring the Models settings forward, for the no-utility-model state. */
  onOpenModels?: () => void;
  /** Open the conversation a record was learned from. */
  onOpenConversation?: (chatId: string) => void;
}) {
  const [settings, setSettings] = useState<MemorySettings | null>(null);
  const [digest, setDigest] = useState<MemoryDigest | null>(null);
  const [records, setRecords] = useState<MemoryRecord[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<Draft | null>(null);
  const { confirm, dialog: confirmDialog } = useConfirm();
  const refreshPresence = useMemoryPresenceStore((state) => state.refresh);
  const applyPresence = useMemoryPresenceStore((state) => state.apply);

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [presence, nextRecords] = await Promise.all([
        refreshPresence(client),
        client.listMemoryRecords(),
      ]);
      setSettings(presence.settings);
      setDigest(presence.digest);
      setRecords(nextRecords);
    } catch (caught) {
      setError(friendlyErrorMessage(caught, "Could not read memory."));
    } finally {
      setLoading(false);
    }
  }, [client, refreshPresence]);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function run(work: () => Promise<void>, fallback: string) {
    setWorking(true);
    setError(null);
    try {
      await work();
    } catch (caught) {
      setError(friendlyErrorMessage(caught, fallback));
    } finally {
      setWorking(false);
    }
  }

  function setEnabled(enabled: boolean) {
    void run(async () => {
      const next = await client.putSettings({
        memory: { enabled, capture_enabled: enabled },
      });
      setSettings(next.memory);
      applyPresence({ settings: next.memory });
      toast.success(enabled ? "Memory is on" : "Memory is off");
    }, "Could not save memory settings.");
  }

  function restore(record: MemoryRecord) {
    void run(async () => {
      await client.setMemoryRecordStatus(record.id, {
        expected_revision: record.revision,
        status: "active",
      });
      toast.success("Restored");
      await reload();
    }, "Could not restore this memory.");
  }

  async function deletePermanently(record: MemoryRecord) {
    const ok = await confirm({
      title: "Delete this memory?",
      description:
        "Tidebreak removes it and its history, and stops using it. It can learn it again if the topic comes up. Backups you made, and the copies Tidebreak saves before updates, still hold it.",
      confirmLabel: "Delete",
      destructive: true,
    });
    if (!ok) return;
    void run(async () => {
      await client.deleteMemoryRecord(record.id);
      toast.success("Deleted");
      await reload();
    }, "Could not delete this memory.");
  }

  async function deleteEverything(count: number) {
    const ok = await confirm({
      title: "Delete every memory?",
      description: `Tidebreak deletes all ${count} ${count === 1 ? "record" : "records"}, forgotten ones included, with their history. Backups you made, and the copies Tidebreak saves before updates, still hold them. This cannot be undone.`,
      confirmLabel: "Delete everything",
      destructive: true,
    });
    if (!ok) return;
    void run(async () => {
      await client.deleteAllMemoryRecords();
      toast.success("Deleted every memory");
      await reload();
    }, "Could not delete every memory.");
  }

  function forget(record: MemoryRecord) {
    void run(async () => {
      await client.setMemoryRecordStatus(record.id, {
        expected_revision: record.revision,
        status: "archived",
      });
      toast.success("Forgotten");
      await reload();
    }, "Could not forget this memory.");
  }

  function save(record: MemoryRecord, title: string, body: string) {
    void run(async () => {
      await client.updateMemoryRecord(record.id, {
        expected_revision: record.revision,
        kind: record.kind,
        title: title.trim(),
        body: body.trim(),
        author: "user",
        origin: record.provenance.origin,
        evidence: record.provenance.evidence,
        links: record.links,
        expires_at: record.expires_at ?? null,
        observation_count: record.observation_count,
      });
      setEditing(null);
      toast.success("Saved");
      await reload();
    }, "Could not save this memory.");
  }

  async function forgetEverything(active: MemoryRecord[]) {
    const ok = await confirm({
      title: "Forget everything?",
      description: `Tidebreak stops using all ${active.length} ${active.length === 1 ? "record" : "records"} in every conversation. They move to Forgotten, where you can restore them, and new memories are still saved.`,
      confirmLabel: "Forget everything",
      destructive: true,
    });
    if (!ok) return;
    void run(async () => {
      for (const record of active) {
        await client.setMemoryRecordStatus(record.id, {
          expected_revision: record.revision,
          status: "archived",
        });
      }
      toast.success("Forgot everything");
      await reload();
    }, "Could not forget everything.");
  }

  const active = records?.filter((record) => record.status === "active") ?? [];
  const forgotten =
    records?.filter((record) => record.status === "archived") ?? [];
  const about = active.filter((record) => groupOf(record.kind) === "about");
  const notes = active.filter((record) => groupOf(record.kind) === "notes");
  const nearlyFull =
    digest != null &&
    digest.byte_cap > 0 &&
    digest.byte_len / digest.byte_cap >= NEARLY_FULL;

  return (
    <SettingsPanel
      title="Memory"
      description="What Tidebreak has learned about you and your work. It saves as you go and uses it in every conversation."
      busy={loading || working}
    >
      {loading && records == null ? (
        <p className="text-sm text-muted-foreground" role="status">
          Loading memory…
        </p>
      ) : records == null || settings == null || digest == null ? (
        <SettingsError title="Could not load memory" onRetry={reload}>
          {error}
        </SettingsError>
      ) : (
        <>
          {!settings.enabled ? (
            <SettingsStatus
              tone="neutral"
              label="Memory is off"
              description="Nothing is saved or used while it is off. Anything already learned is kept."
            />
          ) : !settings.capture_ready ? (
            <div className="flex flex-col items-stretch gap-2">
              <SettingsStatus
                tone="warning"
                label="Memory saves only during conversations"
                description="The end-of-turn review needs a small utility model, and no configured provider serves one. Add a provider, and Tidebreak also reviews each turn for anything worth keeping."
              />
              {onOpenModels && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="self-start"
                  onClick={onOpenModels}
                >
                  Open Models
                </Button>
              )}
            </div>
          ) : nearlyFull ? (
            <SettingsStatus
              tone="warning"
              label="Memory is nearly full"
              description="Tidebreak merges overlapping entries on its own. Forgetting what no longer matters helps."
            />
          ) : null}

          <SettingsSection title="Memory">
            <SettingsField
              label="Remember across conversations"
              hint={
                settings.enabled
                  ? "Turning this off stops saving and using memory. It keeps what is here."
                  : "Nothing is saved or used while this is off."
              }
            >
              <Switch
                checked={settings.enabled}
                disabled={working}
                onCheckedChange={(enabled) => setEnabled(enabled)}
              />
            </SettingsField>
          </SettingsSection>

          {active.length === 0 ? (
            settings.enabled && (
              <SettingsSection title="What Tidebreak knows">
                <Empty className="min-h-40">
                  <EmptyHeader>
                    <EmptyMedia variant="icon" className="text-icon-violet">
                      <Brain />
                    </EmptyMedia>
                    <EmptyTitle>Nothing yet</EmptyTitle>
                    <EmptyDescription>
                      Tell Tidebreak how you like to work, or just keep going.
                      It saves what will matter later and shows you when it
                      does.
                    </EmptyDescription>
                  </EmptyHeader>
                </Empty>
              </SettingsSection>
            )
          ) : (
            <>
              <SettingsSection
                title="About you"
                description="How you like to work. Tidebreak follows these in every conversation."
              >
                <MemoryList
                  records={about}
                  emptyText="Nothing about you yet."
                  editing={editing}
                  working={working}
                  onEdit={setEditing}
                  onSave={save}
                  onForget={forget}
                  onDelete={(record) => void deletePermanently(record)}
                  onOpenConversation={onOpenConversation}
                />
              </SettingsSection>
              <SettingsSection
                title="Notes"
                description="Facts, lessons, and references Tidebreak keeps for later."
              >
                <MemoryList
                  records={notes}
                  emptyText="No notes yet."
                  editing={editing}
                  working={working}
                  onEdit={setEditing}
                  onSave={save}
                  onForget={forget}
                  onDelete={(record) => void deletePermanently(record)}
                  onOpenConversation={onOpenConversation}
                />
              </SettingsSection>
            </>
          )}
          {forgotten.length > 0 && (
            <SettingsSection
              title="Forgotten"
              description="Tidebreak no longer uses these, and does not learn them again right away. Restore one to use it again, or delete it for good."
            >
              <ForgottenList
                records={forgotten}
                working={working}
                onRestore={restore}
                onDelete={(record) => void deletePermanently(record)}
              />
            </SettingsSection>
          )}
          {records.length > 0 && (
            <SettingsSection title="Danger zone">
              {active.length > 0 && (
                <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
                  <div className="min-w-0 flex-1 basis-64">
                    <p className="text-sm font-medium">Forget everything</p>
                    <p className="text-xs text-muted-foreground">
                      Stop using every record. They move to Forgotten, where you
                      can restore them.
                    </p>
                  </div>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={working}
                    onClick={() => void forgetEverything(active)}
                  >
                    Forget everything
                  </Button>
                </div>
              )}
              <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
                <div className="min-w-0 flex-1 basis-64">
                  <p className="text-sm font-medium">Delete everything</p>
                  <p className="text-xs text-muted-foreground">
                    Delete every record, forgotten ones included, with its
                    history. This cannot be undone.
                  </p>
                </div>
                <Button
                  type="button"
                  variant="ghost-destructive"
                  size="sm"
                  disabled={working}
                  onClick={() => void deleteEverything(records.length)}
                >
                  Delete everything
                </Button>
              </div>
            </SettingsSection>
          )}
          {error && <SettingsError>{error}</SettingsError>}
        </>
      )}
      {confirmDialog}
    </SettingsPanel>
  );
}

function MemoryList({
  records,
  emptyText,
  editing,
  working,
  onEdit,
  onSave,
  onForget,
  onDelete,
  onOpenConversation,
}: {
  records: MemoryRecord[];
  emptyText: string;
  editing: Draft | null;
  working: boolean;
  onEdit: (draft: Draft | null) => void;
  onSave: (record: MemoryRecord, title: string, body: string) => void;
  onForget: (record: MemoryRecord) => void;
  onDelete: (record: MemoryRecord) => void;
  onOpenConversation?: (chatId: string) => void;
}) {
  if (records.length === 0) {
    return <p className="text-sm text-muted-foreground">{emptyText}</p>;
  }
  return (
    <ul className="flex flex-col divide-y divide-border">
      {records.map((record) => {
        const draft = editing?.id === record.id ? editing : null;
        const chatId = record.provenance.origin.chat_id;
        return (
          <li key={record.id} className="flex flex-col gap-2 py-3 first:pt-0">
            {draft ? (
              <div className="flex flex-col gap-2">
                <Input
                  aria-label="Memory title"
                  value={draft.title}
                  disabled={working}
                  onChange={(event) =>
                    onEdit({ ...draft, title: event.target.value })
                  }
                />
                <Textarea
                  aria-label="Memory body"
                  value={draft.body}
                  disabled={working}
                  className="min-h-16 text-sm"
                  onChange={(event) =>
                    onEdit({ ...draft, body: event.target.value })
                  }
                />
                <div className="flex flex-wrap gap-2">
                  <Button
                    type="button"
                    size="sm"
                    disabled={
                      working || !draft.title.trim() || !draft.body.trim()
                    }
                    onClick={() => onSave(record, draft.title, draft.body)}
                  >
                    Save
                  </Button>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={working}
                    onClick={() => onEdit(null)}
                  >
                    Cancel
                  </Button>
                </div>
              </div>
            ) : (
              <>
                <div className="min-w-0">
                  <p className="text-sm font-medium">{record.title}</p>
                  <p className="mt-0.5 text-sm whitespace-pre-wrap text-muted-foreground">
                    {record.body}
                  </p>
                </div>
                <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                  <span>
                    {record.provenance.author === "user"
                      ? "You wrote this"
                      : `Learned ${formatDay(record.created_at)}`}
                  </span>
                  {chatId && onOpenConversation && (
                    <button
                      type="button"
                      className="underline-offset-2 hover:underline"
                      onClick={() => onOpenConversation(chatId)}
                    >
                      Open conversation
                    </button>
                  )}
                  <button
                    type="button"
                    className="underline-offset-2 hover:underline"
                    disabled={working}
                    onClick={() =>
                      onEdit({
                        id: record.id,
                        title: record.title,
                        body: record.body,
                      })
                    }
                  >
                    Edit
                  </button>
                  <button
                    type="button"
                    className="underline-offset-2 hover:underline"
                    disabled={working}
                    onClick={() => onForget(record)}
                  >
                    Forget
                  </button>
                  <button
                    type="button"
                    className="underline-offset-2 hover:underline"
                    disabled={working}
                    onClick={() => onDelete(record)}
                  >
                    Delete
                  </button>
                </div>
              </>
            )}
          </li>
        );
      })}
    </ul>
  );
}

/**
 * Forgotten records, collapsed until the person opens them. Each can come
 * back, or go for good.
 */
function ForgottenList({
  records,
  working,
  onRestore,
  onDelete,
}: {
  records: MemoryRecord[];
  working: boolean;
  onRestore: (record: MemoryRecord) => void;
  onDelete: (record: MemoryRecord) => void;
}) {
  const [open, setOpen] = useState(false);
  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger asChild>
        <Button
          type="button"
          variant="link"
          className="h-auto gap-1.5 px-0 py-0.5 font-normal text-muted-foreground hover:text-foreground"
        >
          <ChevronRight
            className={`size-3.5 transition-transform ${open ? "rotate-90" : ""}`}
            aria-hidden="true"
          />
          {open ? "Hide" : "Show"} {records.length} forgotten{" "}
          {records.length === 1 ? "record" : "records"}
        </Button>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <ul className="mt-3 flex flex-col divide-y divide-border">
          {records.map((record) => (
            <li key={record.id} className="flex flex-col gap-2 py-3 first:pt-0">
              <div className="min-w-0">
                <p className="text-sm font-medium">{record.title}</p>
                <p className="mt-0.5 text-sm whitespace-pre-wrap text-muted-foreground">
                  {record.body}
                </p>
              </div>
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                <span>Forgotten {formatDay(record.updated_at)}</span>
                <button
                  type="button"
                  className="underline-offset-2 hover:underline"
                  disabled={working}
                  onClick={() => onRestore(record)}
                >
                  Restore
                </button>
                <button
                  type="button"
                  className="underline-offset-2 hover:underline"
                  disabled={working}
                  onClick={() => onDelete(record)}
                >
                  Delete
                </button>
              </div>
            </li>
          ))}
        </ul>
      </CollapsibleContent>
    </Collapsible>
  );
}

function formatDay(timestamp: string): string {
  const date = new Date(timestamp);
  return Number.isNaN(date.getTime()) ? timestamp : date.toLocaleDateString();
}
