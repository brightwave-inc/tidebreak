import { useCallback, useEffect, useMemo, useState } from "react";
import { Brain, Eye, Search } from "lucide-react";
import { toast } from "sonner";

import type {
  ApiClient,
  MemoryDigest,
  MemorySettings,
  MemoryRecord,
  MemoryRevision,
  MemoryStatus,
  MemorySweepStatus,
} from "../api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Input } from "@/components/ui/input";
import { friendlyErrorMessage } from "@/lib/utils";
import { memoryStatusVariant } from "../memoryStatus";
import { useMemoryPresenceStore } from "../MemoryPresenceStore";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

type MemoryView = "review" | "noticing" | "records";

const VIEW_OPTIONS: { id: MemoryView; label: string }[] = [
  { id: "review", label: "Review" },
  { id: "noticing", label: "Noticing" },
  { id: "records", label: "All records" },
];

/**
 * Memory settings: one switch, then everything the store holds.
 *
 * The switch is the whole setup. Capture follows it on the server, and a
 * captured record never carries authority until it is approved here or in
 * the transcript, so there is no safe reason to make a person find a second
 * switch before anything happens. The status block above the switch names
 * the one thing that is holding memory back, when something is, and the
 * Noticing view shows the patterns capture is watching before they reach
 * review, so the feature reads as alive from the first conversation.
 */
export function MemoryPanel({
  client,
  onOpenModels,
}: {
  client: ApiClient;
  /** Bring the Models settings forward, for the no-utility-model state. */
  onOpenModels?: () => void;
}) {
  const [settings, setSettings] = useState<MemorySettings | null>(null);
  const [view, setView] = useState<MemoryView>("review");
  const [records, setRecords] = useState<MemoryRecord[] | null>(null);
  const [digest, setDigest] = useState<MemoryDigest | null>(null);
  const [search, setSearch] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [revisions, setRevisions] = useState<MemoryRevision[] | null>(null);
  const [sweep, setSweep] = useState<MemorySweepStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refreshPresence = useMemoryPresenceStore((state) => state.refresh);
  const applyPresence = useMemoryPresenceStore((state) => state.apply);

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      // Settings and the digest come through the shared snapshot the
      // activity chip also reads, so the two never race each other and the
      // chip shows what this page just changed.
      const [presence, nextRecords, nextSweep] = await Promise.all([
        refreshPresence(client),
        client.listMemoryRecords(),
        client.getMemorySweepStatus(),
      ]);
      const nextSettings = { memory: presence.settings };
      const nextDigest = presence.digest;
      setSettings(nextSettings.memory);
      setRecords((current) => {
        // First load lands on the view with something to do: review when a
        // proposal waits, noticing when only patterns are being watched,
        // otherwise the full list.
        if (current == null) {
          setView(
            nextRecords.some((record) => record.status === "proposed")
              ? "review"
              : nextRecords.some((record) => record.status === "tracking")
                ? "noticing"
                : "records",
          );
        }
        return nextRecords;
      });
      setDigest(nextDigest);
      setSweep(nextSweep);
      setSelectedId((current) => {
        const exists =
          current != null &&
          nextRecords.some((record) => record.id === current);
        if (exists) return current;
        return (
          nextRecords.find((record) => record.status === "proposed")?.id ??
          nextRecords.find((record) => record.status === "active")?.id ??
          nextRecords[0]?.id ??
          null
        );
      });
    } catch (caught) {
      setError(friendlyErrorMessage(caught, "Could not read memory records."));
    } finally {
      setLoading(false);
    }
  }, [client, refreshPresence]);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    if (selectedId == null) {
      setRevisions(null);
      return;
    }
    let cancelled = false;
    void client
      .getMemoryRevisions(selectedId)
      .then((nextRevisions) => {
        if (!cancelled) setRevisions(nextRevisions);
      })
      .catch((caught) => {
        if (!cancelled) {
          setError(
            friendlyErrorMessage(caught, "Could not read record history."),
          );
        }
      });
    return () => {
      cancelled = true;
    };
  }, [client, selectedId]);

  const filteredRecords = useMemo(() => {
    if (records == null) return null;
    const query = search.trim().toLowerCase();
    if (!query) return records;
    return records.filter(
      (record) =>
        record.title.toLowerCase().includes(query) ||
        record.body.toLowerCase().includes(query),
    );
  }, [records, search]);

  const proposals =
    records?.filter((record) => record.status === "proposed") ?? [];
  const noticing =
    records?.filter((record) => record.status === "tracking") ?? [];
  const visible =
    view === "review"
      ? proposals
      : view === "noticing"
        ? noticing
        : (filteredRecords ?? []);
  // The detail below the list always describes a row in the list above it.
  const selectedRecord =
    visible.find((record) => record.id === selectedId) ?? null;

  function showView(next: MemoryView) {
    setView(next);
    const rows =
      next === "review"
        ? proposals
        : next === "noticing"
          ? noticing
          : (records ?? []);
    setSelectedId((current) =>
      rows.some((record) => record.id === current)
        ? current
        : (rows[0]?.id ?? null),
    );
  }

  async function setStatus(record: MemoryRecord, status: MemoryStatus) {
    setWorking(true);
    setError(null);
    try {
      const updated = await client.setMemoryRecordStatus(record.id, {
        expected_revision: record.revision,
        status,
      });
      toast.success(statusToast(status));
      await reload();
      setSelectedId(updated.id);
    } catch (caught) {
      friendlyStatusError(caught, setError);
    } finally {
      setWorking(false);
    }
  }

  async function updateSettings(update: {
    enabled?: boolean;
    capture_enabled?: boolean;
  }) {
    setWorking(true);
    setError(null);
    try {
      const next = await client.putSettings({ memory: update });
      setSettings(next.memory);
      applyPresence({ settings: next.memory });
      toast.success(
        update.enabled === false
          ? "Memory is off"
          : update.enabled === true
            ? "Memory is on"
            : "Capture resumed",
      );
    } catch (caught) {
      setError(friendlyErrorMessage(caught, "Could not save memory settings."));
    } finally {
      setWorking(false);
    }
  }

  const status =
    settings && digest
      ? memoryStatus(settings, digest, proposals.length, noticing.length)
      : null;

  return (
    <SettingsPanel
      title="Experimental"
      description="Try features that are still changing. Experimental features are off by default."
      busy={loading || working}
    >
      {loading && records == null ? (
        <p className="text-sm text-muted-foreground" role="status">
          Loading memory…
        </p>
      ) : records == null || digest == null || settings == null ? (
        <div className="flex flex-col items-start gap-3">
          <SettingsError>{error}</SettingsError>
          <Button type="button" variant="outline" size="sm" onClick={reload}>
            Try again
          </Button>
        </div>
      ) : (
        <>
          {status && (
            <div className="flex flex-col items-stretch gap-2">
              <SettingsStatus
                tone={status.tone}
                label={status.label}
                description={status.description}
              />
              {status.action === "resume" && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="self-start"
                  disabled={working}
                  onClick={() => void updateSettings({ capture_enabled: true })}
                >
                  Resume capture
                </Button>
              )}
              {status.action === "models" && onOpenModels && (
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
          )}
          {settings.enabled && records.length > 0 && (
            <p className="text-sm text-muted-foreground" role="status">
              {sweepSummary(sweep)}
            </p>
          )}

          <SettingsSection
            title="Memory"
            description="Tidebreak notices facts and preferences after each conversation turn and proposes them here. Only records you approve reach a conversation."
          >
            <SettingsField
              label="Remember across conversations"
              hint={
                settings.enabled
                  ? "Turning this off stops capture and injection. It keeps every record."
                  : "Nothing is captured or injected while this is off. Existing records are kept."
              }
            >
              <Switch
                checked={settings.enabled}
                disabled={working}
                onCheckedChange={(enabled) =>
                  void updateSettings({ enabled, capture_enabled: enabled })
                }
              />
            </SettingsField>
          </SettingsSection>

          {records.length === 0 ? (
            settings.enabled && (
              <SettingsSection title="Records">
                <Empty className="min-h-48">
                  <EmptyHeader>
                    <EmptyMedia variant="icon" className="text-icon-violet">
                      <Brain />
                    </EmptyMedia>
                    <EmptyTitle>Nothing captured yet</EmptyTitle>
                    <EmptyDescription>
                      {settings.capture_ready
                        ? "After each completed turn, Tidebreak proposes what seems worth keeping. A pattern seen once is watched here until it repeats in another conversation."
                        : "Records appear here once capture can run."}
                    </EmptyDescription>
                  </EmptyHeader>
                </Empty>
              </SettingsSection>
            )
          ) : (
            <SettingsSection title="Records">
              <div className="flex flex-wrap items-center gap-2">
                {VIEW_OPTIONS.map((option) => {
                  const count =
                    option.id === "review"
                      ? proposals.length
                      : option.id === "noticing"
                        ? noticing.length
                        : 0;
                  return (
                    <Button
                      key={option.id}
                      type="button"
                      variant={view === option.id ? "default" : "outline"}
                      size="sm"
                      disabled={working}
                      aria-pressed={view === option.id}
                      onClick={() => showView(option.id)}
                    >
                      {option.label}
                      {count > 0 && (
                        <span className="ml-1 tabular-nums opacity-70">
                          {count}
                        </span>
                      )}
                    </Button>
                  );
                })}
              </div>
              {view === "review" ? (
                proposals.length === 0 ? (
                  <Empty className="min-h-48">
                    <EmptyHeader>
                      <EmptyMedia variant="icon" className="text-icon-violet">
                        <Brain />
                      </EmptyMedia>
                      <EmptyTitle>Nothing waiting for review</EmptyTitle>
                      <EmptyDescription>
                        {noticing.length > 0
                          ? `${noticing.length} ${plural(noticing.length, "pattern is", "patterns are")} being watched. Each one moves here once it repeats in another conversation.`
                          : settings.enabled
                            ? "Proposals appear here after a turn teaches something worth keeping."
                            : "Turn memory on, and proposals appear here after a turn teaches something worth keeping."}
                      </EmptyDescription>
                    </EmptyHeader>
                    {noticing.length > 0 && (
                      <EmptyContent>
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={() => showView("noticing")}
                        >
                          See what is being watched
                        </Button>
                      </EmptyContent>
                    )}
                  </Empty>
                ) : (
                  <ul className="flex flex-col gap-2">
                    {proposals.map((record) => (
                      <MemoryRecordRow
                        key={record.id}
                        record={record}
                        selected={record.id === selectedId}
                        onSelect={() => setSelectedId(record.id)}
                      />
                    ))}
                  </ul>
                )
              ) : view === "noticing" ? (
                noticing.length === 0 ? (
                  <Empty className="min-h-48">
                    <EmptyHeader>
                      <EmptyMedia variant="icon" className="text-icon-violet">
                        <Eye />
                      </EmptyMedia>
                      <EmptyTitle>Nothing being watched</EmptyTitle>
                      <EmptyDescription>
                        A pattern seen once waits here until it repeats in
                        another conversation, then moves to review.
                      </EmptyDescription>
                    </EmptyHeader>
                  </Empty>
                ) : (
                  <ul className="flex flex-col gap-2">
                    {noticing.map((record) => (
                      <MemoryRecordRow
                        key={record.id}
                        record={record}
                        selected={record.id === selectedId}
                        onSelect={() => setSelectedId(record.id)}
                      />
                    ))}
                  </ul>
                )
              ) : (
                <div className="flex flex-col gap-3">
                  <label className="flex items-center gap-2 rounded-lg border border-input pr-2 pl-3 text-sm">
                    <Search
                      className="size-4 shrink-0 text-muted-foreground"
                      aria-hidden="true"
                    />
                    <Input
                      className="h-control border-0 bg-transparent pr-0 pl-0 shadow-none focus-visible:border-transparent focus-visible:ring-0"
                      placeholder="Search records…"
                      value={search}
                      onChange={(event) => setSearch(event.target.value)}
                    />
                  </label>
                  {filteredRecords?.length === 0 ? (
                    <Empty className="min-h-48">
                      <EmptyHeader>
                        <EmptyMedia variant="icon" className="text-icon-violet">
                          <Brain />
                        </EmptyMedia>
                        <EmptyTitle>
                          {search.trim()
                            ? "No matching records"
                            : "No records yet"}
                        </EmptyTitle>
                        <EmptyDescription>
                          {search.trim()
                            ? "Try another word or clear the search."
                            : "Every record capture proposes, you approve, or you write lands here."}
                        </EmptyDescription>
                      </EmptyHeader>
                    </Empty>
                  ) : (
                    <ul className="flex flex-col gap-2">
                      {filteredRecords?.map((record) => (
                        <MemoryRecordRow
                          key={record.id}
                          record={record}
                          selected={record.id === selectedId}
                          onSelect={() => setSelectedId(record.id)}
                        />
                      ))}
                    </ul>
                  )}
                </div>
              )}
            </SettingsSection>
          )}

          {selectedRecord && (
            <MemoryDetail
              record={selectedRecord}
              revisions={revisions}
              working={working}
              onApprove={() => void setStatus(selectedRecord, "active")}
              onReview={() => void setStatus(selectedRecord, "proposed")}
              onDismiss={() => void setStatus(selectedRecord, "rejected")}
              onArchive={() => void setStatus(selectedRecord, "archived")}
            />
          )}

          {digest.record_count > 0 && (
            <SettingsSection
              title="Digest preview"
              description="The exact markdown injected at a conversation boundary."
            >
              <div className="flex flex-col gap-3">
                <div
                  className="flex items-center gap-2"
                  role="status"
                  aria-label="Digest size"
                >
                  <div className="h-1.5 min-w-16 grow overflow-hidden rounded-full bg-muted">
                    <div
                      className="h-full rounded-full bg-primary"
                      style={{
                        width: `${Math.min(100, (digest.byte_len / digest.byte_cap) * 100)}%`,
                      }}
                    />
                  </div>
                  <span className="font-mono text-xs text-muted-foreground">
                    {digest.byte_len}/{digest.byte_cap} bytes
                  </span>
                </div>
                <pre className="max-h-64 overflow-auto rounded-lg border bg-muted/40 p-3 font-mono text-xs whitespace-pre-wrap">
                  {digest.markdown}
                </pre>
              </div>
            </SettingsSection>
          )}
          {error && <SettingsError>{error}</SettingsError>}
        </>
      )}
    </SettingsPanel>
  );
}

/**
 * The one line at the top of the page: what memory is doing right now, and
 * when something holds it back, what that is. Each blocker names its own
 * fix, so nobody has to work out which of several conditions failed.
 */
function memoryStatus(
  settings: MemorySettings,
  digest: MemoryDigest,
  proposalCount: number,
  noticingCount: number,
): {
  tone: "ready" | "not-configured" | "disabled";
  label: string;
  description: string;
  /** The one control that clears a blocker, when there is one. */
  action?: "resume" | "models";
} {
  if (!settings.enabled) {
    return {
      tone: "disabled",
      label: "Memory is off",
      description:
        "Turn it on, and Tidebreak proposes records after each conversation turn. Nothing is used until you approve it.",
    };
  }
  if (!settings.capture_enabled) {
    return {
      tone: "not-configured",
      label: "Capture is paused",
      description: `${activeSummary(digest.record_count)} No new records are proposed until capture resumes.`,
      action: "resume",
    };
  }
  if (!settings.capture_ready) {
    return {
      tone: "not-configured",
      label: "Memory is on, but nothing can be captured yet",
      description: `${activeSummary(digest.record_count)} Capture runs on the utility model, and no configured provider serves one. Add a provider with a small model, and capture starts on the next completed turn.`,
      action: "models",
    };
  }
  const pending =
    proposalCount > 0
      ? ` ${proposalCount} ${plural(proposalCount, "proposal waits", "proposals wait")} for your review.`
      : "";
  if (digest.record_count === 0) {
    return {
      tone: "ready",
      label: "Memory is on",
      description:
        proposalCount > 0
          ? `Nothing approved yet.${pending}`
          : noticingCount > 0
            ? `Nothing approved yet. ${noticingCount} ${plural(noticingCount, "pattern is", "patterns are")} being watched until ${noticingCount === 1 ? "it repeats" : "they repeat"} in another conversation.`
            : "Nothing captured yet. Tidebreak reviews each completed turn and proposes what seems worth keeping.",
    };
  }
  return {
    tone: "ready",
    label: `${digest.record_count} active ${plural(digest.record_count, "record reaches", "records reach")} every conversation`,
    description: `Injected as dated claims; the current conversation always overrides them.${pending}`,
  };
}

function activeSummary(count: number): string {
  if (count === 0) return "No approved records yet.";
  return `${count} approved ${plural(count, "record reaches", "records reach")} every conversation.`;
}

function plural(count: number, one: string, many: string): string {
  return count === 1 ? one : many;
}

function statusToast(status: MemoryStatus): string {
  switch (status) {
    case "active":
      return "Memory approved";
    case "proposed":
      return "Sent to review";
    case "rejected":
      return "Memory dismissed";
    case "archived":
      return "Memory archived";
    default:
      return "Memory updated";
  }
}

function friendlyStatusError(
  caught: unknown,
  setError: (message: string) => void,
) {
  setError(friendlyErrorMessage(caught, "Could not change the record status."));
}

/** How often capture has seen a watched pattern, as one short line. */
function observationSummary(record: MemoryRecord): string {
  const seen =
    record.observation_count <= 1
      ? "Seen once"
      : `Seen ${record.observation_count} times`;
  return `${seen} · moves to review when it repeats in another conversation`;
}

function MemoryRecordRow({
  record,
  selected,
  onSelect,
}: {
  record: MemoryRecord;
  selected: boolean;
  onSelect: () => void;
}) {
  const [hovered, setHovered] = useState(false);
  const watched = record.status === "tracking";
  return (
    <li>
      <button
        type="button"
        className={`flex w-full items-start justify-between gap-3 rounded-md border px-3 py-2 text-left transition-colors ${
          selected || hovered ? "border-ring bg-accent" : ""
        }`}
        aria-current={selected ? "true" : undefined}
        onMouseEnter={() => setHovered(true)}
        onMouseLeave={() => setHovered(false)}
        onFocus={() => setHovered(true)}
        onBlur={() => setHovered(false)}
        onClick={onSelect}
      >
        <span className="min-w-0">
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate text-sm font-medium">{record.title}</span>
          </span>
          <span className="mt-1 block truncate text-xs text-muted-foreground">
            {watched ? observationSummary(record) : record.body}
          </span>
        </span>
        <span className="flex shrink-0 items-center gap-2">
          <Badge variant={memoryStatusVariant(record.status)} size="sm">
            {statusLabel(record.status)}
          </Badge>
          <span className="font-mono text-xs text-muted-foreground">
            {formatDay(record.updated_at)}
          </span>
        </span>
      </button>
    </li>
  );
}

/** The user-facing word for a lifecycle state. */
function statusLabel(status: MemoryStatus): string {
  switch (status) {
    case "tracking":
      return "watching";
    case "proposed":
      return "review";
    default:
      return status;
  }
}

function MemoryDetail({
  record,
  revisions,
  working,
  onApprove,
  onReview,
  onDismiss,
  onArchive,
}: {
  record: MemoryRecord;
  revisions: MemoryRevision[] | null;
  working: boolean;
  onApprove: () => void;
  onReview: () => void;
  onDismiss: () => void;
  onArchive: () => void;
}) {
  return (
    <SettingsSection
      title="Record detail"
      description={`${record.kind} · ${record.provenance.author} · revision ${record.revision}`}
    >
      <div className="flex flex-col gap-4">
        <div>
          <h3 className="text-sm font-semibold">{record.title}</h3>
          <p className="mt-2 text-sm whitespace-pre-wrap">{record.body}</p>
        </div>
        <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-xs">
          <dt className="text-muted-foreground">Status</dt>
          <dd>
            <Badge variant={memoryStatusVariant(record.status)} size="sm">
              {statusLabel(record.status)}
            </Badge>
          </dd>
          {record.status === "tracking" && (
            <>
              <dt className="text-muted-foreground">Seen</dt>
              <dd>
                {record.observation_count <= 1
                  ? "once"
                  : `${record.observation_count} times`}
                , first on {formatDay(record.created_at)}
              </dd>
            </>
          )}
          <dt className="text-muted-foreground">Author</dt>
          <dd>{record.provenance.author}</dd>
          {record.provenance.evidence.length > 0 && (
            <>
              <dt className="text-muted-foreground">Evidence</dt>
              <dd className="font-mono">
                {record.provenance.evidence
                  .map((entry) =>
                    entry.kind === "message"
                      ? `message ${entry.message_id}`
                      : `code event ${entry.session_id}:${entry.seq}`,
                  )
                  .join(", ")}
              </dd>
            </>
          )}
          {record.links.length > 0 && (
            <>
              <dt className="text-muted-foreground">Links</dt>
              <dd className="font-mono">
                {record.links
                  .map((link) => `${link.relation} ${link.record_id}`)
                  .join(", ")}
              </dd>
            </>
          )}
          {record.expires_at && (
            <>
              <dt className="text-muted-foreground">Expires</dt>
              <dd>{formatDay(record.expires_at)}</dd>
            </>
          )}
        </dl>
        {record.status === "proposed" && (
          <div className="flex flex-wrap gap-2">
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
              onClick={onDismiss}
            >
              Dismiss
            </Button>
          </div>
        )}
        {record.status === "tracking" && (
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              size="sm"
              disabled={working}
              onClick={onReview}
            >
              Review now
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={working}
              onClick={onDismiss}
            >
              Dismiss
            </Button>
          </div>
        )}
        {record.status === "active" && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="self-start"
            disabled={working}
            onClick={onArchive}
          >
            Archive
          </Button>
        )}
        {revisions != null && revisions.length > 0 && (
          <div>
            <h3 className="text-2xs font-semibold uppercase tracking-wide text-muted-foreground">
              History
            </h3>
            <ol className="mt-2 flex flex-col gap-1">
              {revisions.map((revision) => (
                <li
                  key={revision.id}
                  className="flex items-center justify-between gap-3 text-xs"
                >
                  <span className="truncate">{revision.snapshot.title}</span>
                  <span className="shrink-0 font-mono text-muted-foreground">
                    {formatDay(revision.created_at)} · revision{" "}
                    {revision.ordinal}
                  </span>
                </li>
              ))}
            </ol>
          </div>
        )}
      </div>
    </SettingsSection>
  );
}

function formatDay(timestamp: string): string {
  const date = new Date(timestamp);
  return Number.isNaN(date.getTime()) ? timestamp : date.toLocaleDateString();
}

/** One sentence describing the maintenance sweep's last completed pass. */
function sweepSummary(status: MemorySweepStatus | null): string {
  const run = status?.last_run;
  if (!run) return "Maintenance has not run yet.";
  const parts: string[] = [];
  if (run.expired > 0) {
    parts.push(
      `archived ${run.expired} expired record${run.expired === 1 ? "" : "s"}`,
    );
  }
  if (run.outcome === "proposed") {
    parts.push(
      `proposed ${run.proposed === 1 ? "a merge" : `${run.proposed} merges`} for review`,
    );
  } else if (run.outcome === "declined") {
    parts.push("found nothing to merge");
  } else if (run.outcome === "parked") {
    parts.push("parked until records change");
  } else if (run.outcome === "owner_busy") {
    parts.push("waited while you were working");
  } else if (run.outcome === "no_model") {
    parts.push("skipped consolidation because no utility model is configured");
  } else if (run.outcome === "rate_limited") {
    parts.push("held consolidation for a later pass");
  } else if (parts.length === 0) {
    parts.push("found no changes");
  }
  return `Maintenance last ran ${formatTime(run.ran_at)} and ${parts.join(", ")}.`;
}

/** A local date and time, falling back to the raw string. */
function formatTime(timestamp: string): string {
  const parsed = new Date(timestamp);
  if (Number.isNaN(parsed.getTime())) return timestamp;
  return parsed.toLocaleString();
}
