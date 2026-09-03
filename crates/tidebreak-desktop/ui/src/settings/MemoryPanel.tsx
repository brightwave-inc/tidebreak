import { useCallback, useEffect, useMemo, useState } from "react";
import { Brain, Search } from "lucide-react";
import { toast } from "sonner";

import type {
  ApiClient,
  MemoryDigest,
  MemorySettings,
  MemoryRecord,
  MemoryRevision,
  MemoryScope,
  MemoryStatus,
  MemorySweepStatus,
} from "../api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Input } from "@/components/ui/input";
import { friendlyErrorMessage } from "@/lib/utils";
import { memoryStatusVariant } from "../memoryStatus";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

type MemoryView = "review" | "records";

const VIEW_OPTIONS: { id: MemoryView; label: string }[] = [
  { id: "review", label: "Review" },
  { id: "records", label: "Records" },
];

export function MemoryPanel({ client }: { client: ApiClient }) {
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
  const scope: MemoryScope = { kind: "personal" };

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [nextSettings, nextRecords, nextDigest, nextSweep] =
        await Promise.all([
          client.getSettings(),
          client.listMemoryRecords(),
          client.getMemoryDigest(scope),
          client.getMemorySweepStatus(),
        ]);
      setSettings(nextSettings.memory);
      setRecords(nextRecords);
      setDigest(nextDigest);
      setSweep(nextSweep);
      setSelectedId((current) => {
        const exists =
          current != null &&
          nextRecords.some((record) => record.id === current);
        if (exists) return current;
        return (
          nextRecords.find((record) => record.status === "proposed")?.id ??
          nextRecords[0]?.id ??
          null
        );
      });
    } catch (caught) {
      setError(friendlyErrorMessage(caught, "Could not read memory records."));
    } finally {
      setLoading(false);
    }
  }, [client]);

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

  const selectedRecord =
    records?.find((record) => record.id === selectedId) ?? null;
  const proposals =
    records?.filter((record) => record.status === "proposed") ?? [];

  async function setStatus(record: MemoryRecord, status: MemoryStatus) {
    setWorking(true);
    setError(null);
    try {
      const updated = await client.setMemoryRecordStatus(record.id, {
        expected_revision: record.revision,
        status,
      });
      toast.success(
        status === "active"
          ? "Memory record activated"
          : status === "archived"
            ? "Memory record archived"
            : "Memory record rejected",
      );
      await reload();
      setSelectedId(updated.id);
    } catch (caught) {
      setError(
        friendlyErrorMessage(caught, "Could not change the record status."),
      );
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
      const settings = await client.putSettings({ memory: update });
      setSettings(settings.memory);
      toast.success("Saved memory settings");
    } catch (caught) {
      setError(friendlyErrorMessage(caught, "Could not save memory settings."));
    } finally {
      setWorking(false);
    }
  }

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
      ) : records == null || digest == null ? (
        <div className="flex flex-col items-start gap-3">
          <SettingsError>{error}</SettingsError>
          <Button type="button" variant="outline" size="sm" onClick={reload}>
            Try again
          </Button>
        </div>
      ) : (
        <>
          {settings?.enabled === false ? (
            <SettingsStatus
              tone="disabled"
              label="Memory is off"
              description="Conversations do not receive memory records or memory tools."
            />
          ) : digest.record_count === 0 ? (
            <SettingsStatus
              tone="disabled"
              label="No active memory"
              description="Nothing is injected into conversations yet."
            />
          ) : (
            <SettingsStatus
              tone="ready"
              label={`${digest.record_count} active record${digest.record_count === 1 ? "" : "s"}`}
              description="Personal memory is injected as dated claims; the current conversation always overrides it."
            />
          )}
          {settings?.enabled && (
            <p className="text-sm text-muted-foreground" role="status">
              {sweepSummary(sweep)}
            </p>
          )}

          <SettingsSection
            title="Memory"
            description="Use reviewed records across conversations. Memory stays off until you enable it."
          >
            {settings == null ? (
              <p className="text-sm text-muted-foreground">Loading…</p>
            ) : (
              <>
                <SettingsField
                  label="Enable memory"
                  hint="Allow Tidebreak to use reviewed records and expose memory tools. Turning this off does not delete records."
                >
                  <Switch
                    checked={settings.enabled}
                    disabled={working}
                    onCheckedChange={(enabled) =>
                      void updateSettings({ enabled })
                    }
                  />
                </SettingsField>
                <SettingsField
                  label="Capture new records after turns"
                  hint="Capture uses the utility model and always starts as a proposal."
                >
                  <Switch
                    checked={settings.capture_enabled}
                    disabled={working || !settings.enabled}
                    onCheckedChange={(enabled) =>
                      void updateSettings({ capture_enabled: enabled })
                    }
                  />
                </SettingsField>
                {settings.enabled &&
                  settings.capture_enabled &&
                  !settings.capture_ready && (
                    <SettingsStatus
                      tone="not-configured"
                      label="Capture is waiting on a utility model"
                      description="Choose a model for the utility role, then capture starts on the next completed turn."
                    />
                  )}
              </>
            )}
          </SettingsSection>

          <SettingsSection title="Review">
            <div className="flex flex-wrap items-center gap-2">
              {VIEW_OPTIONS.map((option) => (
                <Button
                  key={option.id}
                  type="button"
                  variant={view === option.id ? "default" : "outline"}
                  size="sm"
                  disabled={working}
                  onClick={() => setView(option.id)}
                >
                  {option.label}
                  {option.id === "review" && proposals.length > 0 && (
                    <span className="ml-1 tabular-nums opacity-70">
                      {proposals.length}
                    </span>
                  )}
                </Button>
              ))}
            </div>
            {view === "review" ? (
              proposals.length === 0 ? (
                <Empty className="min-h-48">
                  <EmptyHeader>
                    <EmptyMedia variant="icon" className="text-icon-violet">
                      <Brain />
                    </EmptyMedia>
                    <EmptyTitle>No memory waiting for review</EmptyTitle>
                    <EmptyDescription>
                      New model-authored records appear here before they can
                      enter a conversation.
                    </EmptyDescription>
                  </EmptyHeader>
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
                      <EmptyTitle>No matching records</EmptyTitle>
                      <EmptyDescription>
                        Try another word or clear the search.
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

          {selectedRecord && (
            <MemoryDetail
              record={selectedRecord}
              revisions={revisions}
              working={working}
              onActivate={() => void setStatus(selectedRecord, "active")}
              onReject={() => void setStatus(selectedRecord, "rejected")}
              onArchive={() => void setStatus(selectedRecord, "archived")}
            />
          )}

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
                {digest.markdown || "No active memory."}
              </pre>
            </div>
          </SettingsSection>
          {error && <SettingsError>{error}</SettingsError>}
        </>
      )}
    </SettingsPanel>
  );
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
  return (
    <li>
      <button
        type="button"
        className={`flex w-full items-start justify-between gap-3 rounded-md border px-3 py-2 text-left transition-colors ${
          selected || hovered ? "border-ring bg-accent" : ""
        }`}
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
            {record.body}
          </span>
        </span>
        <span className="flex shrink-0 items-center gap-2">
          <Badge variant={memoryStatusVariant(record.status)} size="sm">
            {record.status}
          </Badge>
          <span className="font-mono text-xs text-muted-foreground">
            {formatDay(record.updated_at)}
          </span>
        </span>
      </button>
    </li>
  );
}

function MemoryDetail({
  record,
  revisions,
  working,
  onActivate,
  onReject,
  onArchive,
}: {
  record: MemoryRecord;
  revisions: MemoryRevision[] | null;
  working: boolean;
  onActivate: () => void;
  onReject: () => void;
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
              {record.status}
            </Badge>
          </dd>
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
              onClick={onActivate}
            >
              Activate
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={working}
              onClick={onReject}
            >
              Reject
            </Button>
          </div>
        )}
        {record.status === "active" && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={working}
            onClick={onArchive}
          >
            Archive
          </Button>
        )}
        {revisions != null && (
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
