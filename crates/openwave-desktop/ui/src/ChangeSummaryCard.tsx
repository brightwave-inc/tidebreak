import { useEffect, useMemo, useState } from "react";
import {
  AlertTriangle,
  Check,
  FileImage,
  FileText,
  Loader2,
  RotateCcw,
} from "lucide-react";
import type {
  ApiClient,
  ExecFileChangeSummary,
  ExecFileUndoOutcome,
} from "./api";
import type { ExecFilePreviewAvailability } from "./generated/wire";
import { cn } from "./lib/utils";

type ChangeClient = Pick<
  ApiClient,
  "getFileChangePreview" | "undoFileChange" | "undoTurnFileChanges"
>;

type Props = {
  client: ChangeClient;
  chatId: string;
  turnId: string;
  files: ExecFileChangeSummary[];
};

export function ChangeSummaryCard({ client, chatId, turnId, files }: Props) {
  const [rows, setRows] = useState(files);
  const [working, setWorking] = useState<Set<string>>(new Set());
  const [undoingAll, setUndoingAll] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => setRows(files), [files]);

  const available = useMemo(
    () => rows.filter((file) => file.undo === "available"),
    [rows],
  );
  const rejected = rows.filter(
    (file) => file.classification === "rejected",
  ).length;

  function applyOutcomes(outcomes: readonly ExecFileUndoOutcome[]) {
    const byId = new Map(
      outcomes.map((outcome) => [outcome.snapshot_id, outcome.status]),
    );
    setRows((current) =>
      current.map((file) => {
        const status = byId.get(file.snapshot_id);
        if (!status) return file;
        return {
          ...file,
          undo:
            status === "restored" ||
            status === "deleted" ||
            status === "already_undone"
              ? "already_undone"
              : status === "stale"
                ? "stale"
                : "not_available",
          binary_preview: file.binary_preview
            ? {
                ...file.binary_preview,
                after:
                  status === "stale"
                    ? "stale"
                    : status === "restored" ||
                        status === "deleted" ||
                        status === "already_undone"
                      ? "unavailable"
                      : file.binary_preview.after,
              }
            : null,
        };
      }),
    );
  }

  async function undoOne(file: ExecFileChangeSummary) {
    setError(null);
    setWorking((current) => new Set(current).add(file.snapshot_id));
    try {
      applyOutcomes([
        await client.undoFileChange(
          chatId,
          turnId,
          file.snapshot_id,
        ),
      ]);
    } catch {
      setError(`Could not undo ${file.relative_path}.`);
    } finally {
      setWorking((current) => {
        const next = new Set(current);
        next.delete(file.snapshot_id);
        return next;
      });
    }
  }

  async function undoAll() {
    setError(null);
    setUndoingAll(true);
    try {
      const outcome = await client.undoTurnFileChanges(chatId, turnId);
      applyOutcomes(outcome.files);
    } catch {
      setError("Could not undo these changes.");
    } finally {
      setUndoingAll(false);
    }
  }

  return (
    <section
      className="my-2 overflow-hidden rounded-xl border border-border bg-card"
      aria-label="Files changed"
    >
      <header className="flex items-center justify-between gap-3 border-b border-border px-3 py-2.5">
        <div className="min-w-0">
          <div className="flex items-center gap-2 text-sm font-medium">
            <FileText size={15} aria-hidden="true" />
            <span>
              {rows.length} file{rows.length === 1 ? "" : "s"} touched
            </span>
          </div>
          {rejected > 0 && (
            <p className="mt-0.5 text-xs text-destructive">
              {rejected} rejected and left unchanged
            </p>
          )}
        </div>
        <button
          type="button"
          className="inline-flex shrink-0 items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs font-medium hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50"
          disabled={available.length === 0 || undoingAll}
          onClick={() => void undoAll()}
        >
          <RotateCcw size={13} aria-hidden="true" />
          {undoingAll ? "Undoing…" : "Undo all"}
        </button>
      </header>

      <div className="divide-y divide-border">
        {rows.map((file) => (
          <FileChangeRow
            key={file.snapshot_id}
            file={file}
            working={working.has(file.snapshot_id)}
            onUndo={() => void undoOne(file)}
            client={client}
            chatId={chatId}
            turnId={turnId}
          />
        ))}
      </div>
      {error && (
        <p className="border-t border-border px-3 py-2 text-xs text-destructive" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

function FileChangeRow({
  file,
  working,
  onUndo,
  client,
  chatId,
  turnId,
}: {
  file: ExecFileChangeSummary;
  working: boolean;
  onUndo: () => void;
  client: ChangeClient;
  chatId: string;
  turnId: string;
}) {
  const rejected = file.classification === "rejected";
  return (
    <div className={cn("px-3 py-2.5", rejected && "bg-destructive/5")}>
      <div className="flex items-start gap-2">
        {rejected ? (
          <AlertTriangle
            className="mt-0.5 shrink-0 text-destructive"
            size={14}
            aria-hidden="true"
          />
        ) : (
          <Check
            className="mt-0.5 shrink-0 text-muted-foreground"
            size={14}
            aria-hidden="true"
          />
        )}
        <div className="min-w-0 flex-1">
          <p className="truncate font-mono text-xs" title={file.relative_path}>
            {file.relative_path}
          </p>
          <p className="mt-0.5 text-[11px] text-muted-foreground">
            {file.folder_name} · {fileStatus(file)}
          </p>
        </div>
        {!rejected && (
          <button
            type="button"
            className="shrink-0 rounded px-2 py-1 text-xs text-muted-foreground hover:bg-accent hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
            disabled={file.undo !== "available" || working}
            onClick={onUndo}
          >
            {working ? "Undoing…" : file.undo === "already_undone" ? "Undone" : "Undo"}
          </button>
        )}
      </div>
      {file.diff && <TextDiff diff={file.diff} />}
      {file.binary_preview && (
        <BinaryPreview
          client={client}
          chatId={chatId}
          turnId={turnId}
          file={file}
          preview={file.binary_preview}
        />
      )}
      {!rejected && !file.diff && !file.binary_preview && (
        <p className="ml-6 mt-2 text-xs text-muted-foreground">
          No change preview is available for this file type or revision.
        </p>
      )}
    </div>
  );
}

function TextDiff({ diff }: { diff: string }) {
  return (
    <details className="ml-6 mt-2">
      <summary className="cursor-pointer select-none text-xs text-muted-foreground hover:text-foreground">
        Text diff
      </summary>
      <pre className="mt-2 max-h-72 overflow-auto rounded-md border border-border bg-muted/40 py-2 text-[11px] leading-4">
        {diff.split("\n").map((line, index) => (
          <span
            // A unified diff can contain identical lines; position is the
            // stable identity within this immutable presentation.
            key={index}
            className={cn(
              "block min-h-4 px-2",
              line.startsWith("+") &&
                !line.startsWith("+++") &&
                "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
              line.startsWith("-") &&
                !line.startsWith("---") &&
                "bg-red-500/10 text-red-700 dark:text-red-300",
              line.startsWith("@@") && "text-muted-foreground",
            )}
          >
            {line || " "}
          </span>
        ))}
      </pre>
    </details>
  );
}

function BinaryPreview({
  client,
  chatId,
  turnId,
  file,
  preview,
}: {
  client: ChangeClient;
  chatId: string;
  turnId: string;
  file: ExecFileChangeSummary;
  preview: NonNullable<ExecFileChangeSummary["binary_preview"]>;
}) {
  const [open, setOpen] = useState(false);
  return (
    <details
      className="ml-6 mt-2"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="flex cursor-pointer select-none items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground">
        <FileImage size={13} aria-hidden="true" />
        Before and after preview
      </summary>
      <div className="mt-2 grid gap-2 md:grid-cols-2">
        <RevisionPreview
          client={client}
          chatId={chatId}
          turnId={turnId}
          snapshotId={file.snapshot_id}
          revision="before"
          availability={preview.before}
          active={open}
          fileName={file.relative_path}
        />
        <RevisionPreview
          client={client}
          chatId={chatId}
          turnId={turnId}
          snapshotId={file.snapshot_id}
          revision="after"
          availability={preview.after}
          active={open}
          fileName={file.relative_path}
        />
      </div>
    </details>
  );
}

type LoadedRevision =
  | { status: "idle" | "loading" }
  | { status: "loaded"; url: string }
  | { status: "error"; message: string };

function RevisionPreview({
  client,
  chatId,
  turnId,
  snapshotId,
  revision,
  availability,
  active,
  fileName,
}: {
  client: ChangeClient;
  chatId: string;
  turnId: string;
  snapshotId: string;
  revision: "before" | "after";
  availability: ExecFilePreviewAvailability;
  active: boolean;
  fileName: string;
}) {
  const [loaded, setLoaded] = useState<LoadedRevision>({ status: "idle" });

  useEffect(() => {
    if (!active || availability !== "available") {
      setLoaded({ status: "idle" });
      return;
    }
    const controller = new AbortController();
    let objectUrl: string | null = null;
    setLoaded({ status: "loading" });
    void client
      .getFileChangePreview(
        chatId,
        turnId,
        snapshotId,
        revision,
        controller.signal,
      )
      .then((blob) => {
        objectUrl = URL.createObjectURL(blob);
        setLoaded({ status: "loaded", url: objectUrl });
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        const message =
          error instanceof Error
            ? error.message.replace(/^\d+:\s*/, "")
            : "Preview unavailable.";
        setLoaded({ status: "error", message });
      });
    return () => {
      controller.abort();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [
    active,
    availability,
    chatId,
    client,
    revision,
    snapshotId,
    turnId,
  ]);

  const label = revision === "before" ? "Before" : "After";
  return (
    <figure className="overflow-hidden rounded-md border border-border bg-muted/20">
      <figcaption className="border-b border-border px-2 py-1.5 text-[11px] font-medium text-muted-foreground">
        {label}
      </figcaption>
      <div className="flex min-h-36 items-center justify-center p-2">
        {availability === "empty" ? (
          <p className="text-xs text-muted-foreground">
            {revision === "before" ? "No previous file" : "File deleted"}
          </p>
        ) : availability === "stale" ? (
          <PreviewNotice>Changed again; preview unavailable</PreviewNotice>
        ) : availability === "too_large" ? (
          <PreviewNotice>Too large to preview</PreviewNotice>
        ) : availability === "unavailable" ? (
          <PreviewNotice>Preview unavailable</PreviewNotice>
        ) : loaded.status === "loaded" ? (
          <img
            className="max-h-80 w-full object-contain"
            src={loaded.url}
            alt={`${label} preview of ${fileName}`}
          />
        ) : loaded.status === "error" ? (
          <PreviewNotice>{loaded.message}</PreviewNotice>
        ) : (
          <Loader2
            className="animate-spin text-muted-foreground"
            size={18}
            aria-label={`Loading ${revision} preview`}
          />
        )}
      </div>
    </figure>
  );
}

function PreviewNotice({ children }: { children: React.ReactNode }) {
  return (
    <p className="max-w-48 text-center text-xs text-muted-foreground">
      {children}
    </p>
  );
}

function fileStatus(file: ExecFileChangeSummary): string {
  if (file.classification === "rejected") {
    return (
      {
        stale: "Rejected: file changed before write-back",
        snapshot_unavailable: "Rejected: undo snapshot unavailable",
        staged_file_too_large: "Rejected: staged file is too large",
        trash_unavailable: "Rejected: file could not be moved to trash",
        unavailable: "Rejected: file could not be written safely",
      }[file.rejection_reason ?? "unavailable"] ?? "Rejected"
    );
  }
  if (file.undo === "already_undone") return "Undone";
  if (file.undo === "stale") return "Changed again; undo unavailable";
  if (file.undo === "not_available") return "Undo unavailable";
  return (
    {
      created: "Created",
      overwritten: "Modified",
      deleted: "Deleted",
    }[file.change ?? "overwritten"] ?? "Changed"
  );
}
