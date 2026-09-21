import { useEffect, useState } from "react";
import { Brain } from "lucide-react";

import type { ApiClient, MemoryRecord } from "./api";
import type { TurnId } from "./generated/wire";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { friendlyErrorMessage } from "@/lib/utils";
import { ToolCardShell } from "./ToolCardShell";

/** What editing or forgetting a remembered record needs from the client. */
export type MemoryRememberedClient = Pick<
  ApiClient,
  "setMemoryRecordStatus" | "updateMemoryRecord"
>;

type MemoryRememberedCardProps = {
  turnId: TurnId;
  /** The turn's model-authored records, in the order the server retained. */
  records: MemoryRecord[];
  client: MemoryRememberedClient;
};

/** A record's own draft while its title and body are being edited inline. */
type Draft = { id: string; title: string; body: string };

/**
 * What one turn remembered, as an expandable transcript row.
 *
 * A remembered record is live the moment it is written (decision 0099), so
 * this row asks for nothing. It shows what was kept and offers the two
 * controls that matter after the fact: Edit, which rewrites the entry, and
 * Forget, which archives it. Every mutation sends `expected_revision` from
 * the record currently held and replaces it with the server's returned
 * record, so a change made elsewhere surfaces as a conflict instead of
 * silently overwriting it.
 */
export function MemoryRememberedCard({
  records,
  client,
}: MemoryRememberedCardProps) {
  const [rows, setRows] = useState(records);
  const [working, setWorking] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [draft, setDraft] = useState<Draft | null>(null);

  useEffect(() => setRows(records), [records]);

  const kept = rows.filter((record) => record.status === "active");

  function replaceRow(updated: MemoryRecord) {
    setRows((current) =>
      current.map((row) => (row.id === updated.id ? updated : row)),
    );
  }

  async function mutate(
    record: MemoryRecord,
    run: () => Promise<MemoryRecord>,
    fallback: string,
  ) {
    setWorking((current) => new Set(current).add(record.id));
    setErrors(({ [record.id]: _, ...rest }) => rest);
    try {
      replaceRow(await run());
      setDraft((current) => (current?.id === record.id ? null : current));
    } catch (caught) {
      setErrors((current) => ({
        ...current,
        [record.id]: friendlyErrorMessage(caught, fallback),
      }));
    } finally {
      setWorking((current) => {
        const next = new Set(current);
        next.delete(record.id);
        return next;
      });
    }
  }

  function forget(record: MemoryRecord) {
    void mutate(
      record,
      () =>
        client.setMemoryRecordStatus(record.id, {
          expected_revision: record.revision,
          status: "archived",
        }),
      "Could not forget this memory.",
    );
  }

  function saveDraft(record: MemoryRecord, next: Draft) {
    void mutate(
      record,
      () =>
        client.updateMemoryRecord(record.id, {
          expected_revision: record.revision,
          kind: record.kind,
          title: next.title,
          body: next.body,
          author: record.provenance.author,
          origin: record.provenance.origin,
          evidence: record.provenance.evidence,
          links: record.links,
          expires_at: record.expires_at ?? null,
          observation_count: record.observation_count,
        }),
      "Could not save this memory.",
    );
  }

  const title =
    kept.length === 0
      ? "Memory forgotten"
      : kept.length === 1
        ? `Remembered: ${kept[0].title}`
        : `Remembered ${kept.length} things`;

  return (
    <ToolCardShell
      icon={<Brain className="text-icon-violet" aria-hidden="true" />}
      title={title}
      label="Remembered"
    >
      <ul className="flex flex-col gap-2">
        {rows.map((record) => (
          <MemoryRememberedRow
            key={record.id}
            record={record}
            working={working.has(record.id)}
            error={errors[record.id]}
            draft={draft?.id === record.id ? draft : null}
            onDraftChange={setDraft}
            onForget={() => forget(record)}
            onSave={(next) => saveDraft(record, next)}
          />
        ))}
      </ul>
    </ToolCardShell>
  );
}

function MemoryRememberedRow({
  record,
  working,
  error,
  draft,
  onDraftChange,
  onForget,
  onSave,
}: {
  record: MemoryRecord;
  working: boolean;
  error?: string;
  draft: Draft | null;
  onDraftChange: (draft: Draft | null) => void;
  onForget: () => void;
  onSave: (draft: Draft) => void;
}) {
  const live = record.status === "active";
  return (
    <li className="rounded-md border border-border px-3 py-2">
      {draft ? (
        <div className="flex flex-col gap-2">
          <Input
            aria-label="Memory title"
            value={draft.title}
            disabled={working}
            onChange={(event) =>
              onDraftChange({ ...draft, title: event.target.value })
            }
          />
          <Textarea
            aria-label="Memory body"
            value={draft.body}
            disabled={working}
            className="min-h-16 text-xs"
            onChange={(event) =>
              onDraftChange({ ...draft, body: event.target.value })
            }
          />
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              size="sm"
              disabled={working || !draft.title.trim() || !draft.body.trim()}
              onClick={() => onSave(draft)}
            >
              {working ? "Saving…" : "Save"}
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={working}
              onClick={() => onDraftChange(null)}
            >
              Cancel
            </Button>
          </div>
        </div>
      ) : (
        <>
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <p
                className={`text-sm font-medium ${live ? "" : "text-muted-foreground line-through"}`}
              >
                {record.title}
              </p>
              <p className="mt-0.5 whitespace-pre-wrap text-xs text-muted-foreground">
                {record.body}
              </p>
            </div>
            {!live && (
              <span className="shrink-0 text-xs text-muted-foreground">
                Forgotten
              </span>
            )}
          </div>
          {live && (
            <div className="mt-2 flex flex-wrap gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={working}
                onClick={() =>
                  onDraftChange({
                    id: record.id,
                    title: record.title,
                    body: record.body,
                  })
                }
              >
                Edit
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                disabled={working}
                onClick={onForget}
              >
                Forget
              </Button>
            </div>
          )}
        </>
      )}
      {error && (
        <p className="mt-1 text-xs text-destructive" role="alert">
          {error}
        </p>
      )}
    </li>
  );
}
