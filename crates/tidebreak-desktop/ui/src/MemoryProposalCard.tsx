import { useEffect, useState } from "react";
import { Brain } from "lucide-react";

import type { ApiClient, MemoryRecord } from "./api";
import type { TurnId } from "./generated/wire";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { friendlyErrorMessage } from "@/lib/utils";
import { memoryStatusVariant } from "./memoryStatus";
import { ToolCardShell } from "./ToolCardShell";

/** What deciding or editing a proposal needs from the connected client. */
export type MemoryProposalClient = Pick<
  ApiClient,
  "setMemoryRecordStatus" | "updateMemoryRecord"
>;

type MemoryProposalCardProps = {
  turnId: TurnId;
  /** The turn's model-authored records, in the order the server retained. */
  records: MemoryRecord[];
  client: MemoryProposalClient;
};

/** A record's own draft while its title and body are being edited inline. */
type Draft = { id: string; title: string; body: string };

/**
 * One turn's memory proposals, as an expandable transcript row.
 *
 * Deliberately not an ApprovalCard: a proposal is a record with a lifecycle,
 * not a parked consent. Nothing is blocked on the reader — the turn already
 * finished, the record sits in `proposed` until someone reviews it here or in
 * settings, and a decision is an ordinary CAS mutation that can be revisited.
 * ApprovalCard also auto-focuses and is bound to tool-call ids, neither of
 * which fits a row that merely reports what a turn produced.
 *
 * Every mutation sends `expected_revision` from the record currently held and
 * replaces it with the server's returned record, so a decision made elsewhere
 * surfaces as a conflict error instead of silently overwriting it.
 */
export function MemoryProposalCard({
  records,
  client,
}: MemoryProposalCardProps) {
  const [rows, setRows] = useState(records);
  const [working, setWorking] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [draft, setDraft] = useState<Draft | null>(null);

  useEffect(() => setRows(records), [records]);

  const pending = rows.filter((record) => record.status === "proposed").length;

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

  function decide(record: MemoryRecord, status: "active" | "rejected") {
    void mutate(
      record,
      () =>
        client.setMemoryRecordStatus(record.id, {
          expected_revision: record.revision,
          status,
        }),
      status === "active"
        ? "Could not approve this memory record."
        : "Could not dismiss this memory record.",
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
      "Could not save this memory record.",
    );
  }

  return (
    <ToolCardShell
      icon={<Brain className="text-icon-violet" aria-hidden="true" />}
      title={
        pending > 0
          ? `${pending} memory proposal${pending === 1 ? "" : "s"}`
          : "Memory proposals reviewed"
      }
      trailing={
        pending > 0 ? (
          <Badge variant="warning" size="sm">
            {pending} pending
          </Badge>
        ) : undefined
      }
      label="Memory proposals"
    >
      <ul className="flex flex-col gap-2">
        {rows.map((record) => (
          <MemoryProposalRow
            key={record.id}
            record={record}
            working={working.has(record.id)}
            error={errors[record.id]}
            draft={draft?.id === record.id ? draft : null}
            onDraftChange={setDraft}
            onApprove={() => decide(record, "active")}
            onDismiss={() => decide(record, "rejected")}
            onSave={(next) => saveDraft(record, next)}
          />
        ))}
      </ul>
    </ToolCardShell>
  );
}

function MemoryProposalRow({
  record,
  working,
  error,
  draft,
  onDraftChange,
  onApprove,
  onDismiss,
  onSave,
}: {
  record: MemoryRecord;
  working: boolean;
  error?: string;
  draft: Draft | null;
  onDraftChange: (draft: Draft | null) => void;
  onApprove: () => void;
  onDismiss: () => void;
  onSave: (draft: Draft) => void;
}) {
  const proposed = record.status === "proposed";
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
              <p className="text-sm font-medium">{record.title}</p>
              <p className="mt-0.5 text-xs text-muted-foreground">
                {record.kind}
              </p>
            </div>
            {!proposed && (
              <Badge
                variant={memoryStatusVariant(record.status)}
                size="sm"
                className="shrink-0"
              >
                {record.status}
              </Badge>
            )}
          </div>
          <p className="mt-1 whitespace-pre-wrap text-xs text-muted-foreground">
            {record.body}
          </p>
          {proposed && (
            <div className="mt-2 flex flex-wrap gap-2">
              <Button
                type="button"
                size="sm"
                disabled={working}
                onClick={onApprove}
              >
                Approve
              </Button>
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
                onClick={onDismiss}
              >
                Dismiss
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
