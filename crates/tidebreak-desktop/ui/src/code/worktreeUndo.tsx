import { type ReactElement, type ReactNode, useCallback } from "react";
import { toast } from "sonner";

import { HttpError, type ApiClient } from "../api/client";
import type {
  CheckpointRestoreTarget,
  CodeCheckpointRestorePreview,
  CodeFileChange,
  FileChangeKind,
} from "../api/types";
import type { CodeRestoreAffectedTurn } from "../generated/wire";
import { type ConfirmOptions, useConfirm } from "@/components/ConfirmDialog";
import { friendlyErrorMessage } from "@/lib/utils";
import { noteWorkspaceFilesChanged } from "./CodeUpdatesStore";
import { HARNESS_LABELS } from "./labels";
import { STATUS_TEXT } from "./statusTone";
import type { DiffHunk } from "./unifiedDiff";

/**
 * Undo in the worktree: restore to before a turn, revert one file or hunk,
 * and discard uncommitted changes.
 *
 * Every action here changes files on disk, so each one asks first with a
 * confirmation that names what is lost, and the server applies it. The
 * renderer never edits text. The server refuses all of them while a turn
 * runs; the surfaces that offer them also turn them off, with a reason, while
 * they can see one running.
 */

export type WorktreeUndoClient = Pick<
  ApiClient,
  | "previewCodeCheckpointRestore"
  | "restoreCodeCheckpoint"
  | "revertCodeWorkspaceChange"
  | "discardCodeWorkspaceChanges"
>;

/** The sentence a surface shows while a turn holds the worktree. */
export const TURN_RUNNING_REASON = "Wait for the turn to finish.";

/** How many changed files a confirmation lists before it says "and N more". */
const LISTED_FILES = 6;

/** The file-change letters the Changes list uses, for the same files. */
export const FILE_KIND: Record<
  FileChangeKind,
  { letter: string; label: string; className: string }
> = {
  added: { letter: "A", label: "Added", className: STATUS_TEXT.ready },
  modified: { letter: "M", label: "Modified", className: STATUS_TEXT.warning },
  deleted: { letter: "D", label: "Deleted", className: STATUS_TEXT.critical },
  renamed: { letter: "R", label: "Renamed", className: STATUS_TEXT.pending },
};

/** What a revert names: a file in the diff being read, and whose diff it is. */
export type RevertRequest = {
  path: string;
  /** The turn whose diff is open. Absent for the workspace against its base. */
  turnId?: string;
  /** What the Changes list knows about the file, when it is known. */
  kind?: FileChangeKind;
  previousPath?: string;
};

export function fileName(path: string): string {
  return path.split("/").filter(Boolean).at(-1) ?? path;
}

/**
 * The question before a restore, and the losses it names.
 *
 * `preview.files` is everything that changed since the target state: the
 * turn's own work, later turns, other agents, and edits made by hand.
 * `preview.affected_turns` names the other agents' turns it undoes.
 */
export function restoreConfirmation(
  preview: CodeCheckpointRestorePreview,
  options: { turnOrdinal?: number | null; redo?: boolean } = {},
): ConfirmOptions {
  const undo = preview.target.kind === "before_restore";
  const title = undo
    ? options.redo
      ? "Redo the restore?"
      : "Undo this restore?"
    : options.turnOrdinal
      ? `Restore to before turn ${options.turnOrdinal}?`
      : "Restore to before this turn?";
  const lead = undo
    ? "The workspace goes back to how it was just before the restore."
    : "The workspace goes back to how it was before this turn started.";
  return {
    title,
    description: (
      <>
        {lead} These changes are lost, including edits you made by hand:
        <ChangedFileList
          files={preview.files}
          total={Math.max(preview.stat.files, preview.files.length)}
        />
        {preview.affected_turns.length > 0 && (
          <AffectedTurnList turns={preview.affected_turns} undo={undo} />
        )}
        <span className="mt-3 block">
          The agent still remembers the turns this undoes. Tidebreak tells it
          which files changed when you send your next message.{" "}
          {undo
            ? "The restore you undo stays in the conversation."
            : "You can undo the restore from the conversation."}
        </span>
      </>
    ),
    confirmLabel: undo ? (options.redo ? "Redo" : "Undo restore") : "Restore",
    destructive: true,
  };
}

/**
 * Why a restore cannot run yet: files no snapshot holds, most often ignored
 * ones, stand where it would write. Confirming checks again.
 */
export function restoreBlockedNotice(
  preview: Pick<CodeCheckpointRestorePreview, "blocked">,
): ConfirmOptions {
  return {
    title: "Move these files first",
    description: (
      <>
        The restore would overwrite or remove these files, and no undo could
        bring them back. Your <FilePath path=".gitignore" /> keeps most such
        files out of every checkpoint.
        <PathList paths={preview.blocked} />
        <span className="mt-3 block">
          Move them out of the workspace, then check again.
        </span>
      </>
    ),
    confirmLabel: "Check again",
    cancelLabel: "Close",
  };
}

/** The question before a whole-file revert. */
export function revertFileConfirmation(request: RevertRequest): ConfirmOptions {
  const name = fileName(request.path);
  if (request.turnId) {
    const consequence =
      request.kind === "added" ? (
        <>
          <FilePath path={request.path} /> is new in this turn, so reverting it
          deletes it. If it changed after the turn, Tidebreak leaves it alone.
        </>
      ) : request.kind === "deleted" ? (
        <>
          <FilePath path={request.path} /> comes back as it was before this
          turn.
        </>
      ) : request.kind === "renamed" && request.previousPath ? (
        <>
          <FilePath path={request.path} /> goes back to its old name,{" "}
          <FilePath path={request.previousPath} />, without this turn&apos;s
          changes. Changes made after the turn stay.
        </>
      ) : (
        <>
          Tidebreak undoes what this turn changed in{" "}
          <FilePath path={request.path} />. Changes made after the turn stay.
        </>
      );
    return {
      title: `Revert this turn's changes to ${name}?`,
      description: (
        <>
          {consequence} If a later change touches the same lines, nothing is
          reverted. This cannot be undone.
        </>
      ),
      confirmLabel: "Revert file",
      destructive: true,
    };
  }
  const consequence =
    request.kind === "added" ? (
      <>
        <FilePath path={request.path} /> is new in this workspace, so reverting
        it deletes it.
      </>
    ) : request.kind === "deleted" ? (
      <>
        <FilePath path={request.path} /> comes back as it is on the base branch.
      </>
    ) : request.kind === "renamed" && request.previousPath ? (
      <>
        <FilePath path={request.path} /> goes back to its old name,{" "}
        <FilePath path={request.previousPath} />, and to its version on the base
        branch.
      </>
    ) : (
      <>
        <FilePath path={request.path} /> goes back to its version on the base
        branch.
      </>
    );
  return {
    title: `Revert ${name}?`,
    description: (
      <>
        {consequence} Every change to it in this workspace is lost, including
        edits you made by hand. This cannot be undone.
      </>
    ),
    confirmLabel: "Revert file",
    destructive: true,
  };
}

/** The question before one hunk goes back. */
export function revertHunkConfirmation(
  request: RevertRequest,
  hunk: DiffHunk,
): ConfirmOptions {
  const since = request.turnId ? "this turn" : "the base branch";
  if (request.kind === "added" || request.kind === "deleted") {
    return {
      title: "Revert this change?",
      description: (
        <>
          This change is the whole file.{" "}
          {request.kind === "added" ? (
            <>
              <FilePath path={request.path} /> is new since {since}, so
              reverting it deletes the file.
            </>
          ) : (
            <>
              <FilePath path={request.path} /> comes back as it was before{" "}
              {since}.
            </>
          )}{" "}
          This cannot be undone.
        </>
      ),
      confirmLabel: request.kind === "added" ? "Delete file" : "Revert change",
      destructive: true,
    };
  }
  const where =
    hunk.newCount > 0
      ? hunk.newCount === 1
        ? `Line ${hunk.newStart}`
        : `Lines ${hunk.newStart}–${hunk.newStart + hunk.newCount - 1}`
      : `The lines removed near line ${Math.max(hunk.newStart, 1)}`;
  return {
    title: "Revert this change?",
    description: (
      <>
        {where} of <FilePath path={request.path} />{" "}
        {hunk.newCount === 0 ? "come back" : "go back"} to how{" "}
        {hunk.newCount === 1 ? "it was" : "they were"}{" "}
        {request.turnId ? "before this turn" : "on the base branch"}. Nothing
        else in the file changes. This cannot be undone.
      </>
    ),
    confirmLabel: "Revert change",
    destructive: true,
  };
}

/** The question before a file's uncommitted changes are thrown away. */
export function discardConfirmation(
  file: Pick<CodeFileChange, "path" | "kind" | "previous_path">,
): ConfirmOptions {
  const name = fileName(file.path);
  return {
    title: `Discard changes to ${name}?`,
    description:
      file.kind === "added" ? (
        <>
          <FilePath path={file.path} /> has never been committed, so discarding
          it deletes it. This cannot be undone.
        </>
      ) : (
        <>
          <FilePath path={file.path} /> goes back to its last commit
          {file.kind === "renamed" && file.previous_path ? (
            <>
              , under <FilePath path={file.previous_path} /> if the rename is
              not committed yet
            </>
          ) : null}
          . Uncommitted changes to it are lost, including edits you made by
          hand. This cannot be undone.
        </>
      ),
    confirmLabel: "Discard",
    destructive: true,
  };
}

/** Why an undo failed, in words the person can act on. */
export function undoFailureMessage(error: unknown, fallback: string): string {
  if (error instanceof HttpError && error.kind === "diff_changed") {
    return "The diff changed since you opened it. Review it again, then revert.";
  }
  return friendlyErrorMessage(error, fallback);
}

/**
 * The confirm-then-apply flows for one workspace, with the one dialog they
 * share. The page renders `dialog` once and hands the actions to the
 * transcript, the diff, and Source control.
 */
export function useWorktreeUndo({
  client,
  workspaceId,
}: {
  client: WorktreeUndoClient;
  workspaceId: string;
}): {
  restore: (
    target: CheckpointRestoreTarget,
    options?: { turnOrdinal?: number | null; redo?: boolean },
  ) => Promise<void>;
  revertFile: (request: RevertRequest) => Promise<boolean>;
  revertHunk: (request: RevertRequest, hunk: DiffHunk) => Promise<boolean>;
  discard: (
    file: Pick<CodeFileChange, "path" | "kind" | "previous_path">,
    expectedTree?: string,
  ) => Promise<boolean>;
  dialog: ReactElement;
} {
  const { confirm, dialog } = useConfirm();

  const restore = useCallback(
    async (
      target: CheckpointRestoreTarget,
      options: { turnOrdinal?: number | null; redo?: boolean } = {},
    ): Promise<void> => {
      let preview: CodeCheckpointRestorePreview;
      try {
        preview = await client.previewCodeCheckpointRestore(
          workspaceId,
          target,
        );
      } catch (error) {
        toast.error(
          friendlyErrorMessage(
            error,
            "Could not read what the restore changes",
          ),
        );
        return;
      }
      if (preview.files.length === 0) {
        toast.message("The workspace already matches that state.");
        return;
      }
      if (preview.blocked.length > 0) {
        if (await confirm(restoreBlockedNotice(preview))) {
          await restore(target, options);
        }
        return;
      }
      if (!(await confirm(restoreConfirmation(preview, options)))) return;
      try {
        const restored = await client.restoreCodeCheckpoint(
          workspaceId,
          target,
          preview.current_tree,
        );
        noteWorkspaceFilesChanged(workspaceId);
        const count = restored.stat.files;
        toast.success(`Restored ${count} ${count === 1 ? "file" : "files"}`, {
          action: {
            label: "Undo",
            onClick: () =>
              void restore({
                kind: "before_restore",
                restore_id: restored.restore_id,
              }),
          },
        });
      } catch (error) {
        if (error instanceof HttpError && error.kind === "worktree_changed") {
          toast.error(
            "The workspace changed while you were reviewing the restore.",
            {
              action: {
                label: "Review again",
                onClick: () => void restore(target, options),
              },
            },
          );
          return;
        }
        if (error instanceof HttpError && error.kind === "restore_failed") {
          // The conversation shows how it ended, with its Undo.
          noteWorkspaceFilesChanged(workspaceId);
        }
        toast.error(friendlyErrorMessage(error, "Could not restore"));
      }
    },
    [client, confirm, workspaceId],
  );

  const revertFile = useCallback(
    async (request: RevertRequest): Promise<boolean> => {
      if (!(await confirm(revertFileConfirmation(request)))) return false;
      try {
        await client.revertCodeWorkspaceChange(workspaceId, {
          path: request.path,
          ...(request.turnId ? { turn_id: request.turnId } : {}),
        });
      } catch (error) {
        toast.error(undoFailureMessage(error, "Could not revert the file"));
        return false;
      }
      noteWorkspaceFilesChanged(workspaceId);
      toast.success(`Reverted ${fileName(request.path)}`);
      return true;
    },
    [client, confirm, workspaceId],
  );

  const revertHunk = useCallback(
    async (request: RevertRequest, hunk: DiffHunk): Promise<boolean> => {
      if (!(await confirm(revertHunkConfirmation(request, hunk)))) return false;
      try {
        await client.revertCodeWorkspaceChange(workspaceId, {
          path: request.path,
          ...(request.turnId ? { turn_id: request.turnId } : {}),
          hunk: { index: hunk.index, text: hunk.text },
        });
      } catch (error) {
        toast.error(undoFailureMessage(error, "Could not revert the change"));
        return false;
      }
      noteWorkspaceFilesChanged(workspaceId);
      toast.success("Reverted the change");
      return true;
    },
    [client, confirm, workspaceId],
  );

  const discard = useCallback(
    async (
      file: Pick<CodeFileChange, "path" | "kind" | "previous_path">,
      expectedTree?: string,
    ): Promise<boolean> => {
      if (!(await confirm(discardConfirmation(file)))) return false;
      try {
        // The file as the list names it. The server puts a renamed file
        // back under its committed name, whether or not the rename itself
        // was committed.
        await client.discardCodeWorkspaceChanges(
          workspaceId,
          [file.path],
          expectedTree,
        );
      } catch (error) {
        toast.error(undoFailureMessage(error, "Could not discard the changes"));
        return false;
      }
      noteWorkspaceFilesChanged(workspaceId);
      toast.success(`Discarded changes to ${fileName(file.path)}`);
      return true;
    },
    [client, confirm, workspaceId],
  );

  return { restore, revertFile, revertHunk, discard, dialog };
}

function FilePath({ path }: { path: string }): ReactNode {
  return (
    <span className="text-foreground break-all font-mono text-xs">{path}</span>
  );
}

/** The files a restore undoes, bounded, in the Changes list's own letters. */
function ChangedFileList({
  files,
  total,
}: {
  files: readonly CodeFileChange[];
  total: number;
}) {
  const shown = files.slice(0, LISTED_FILES);
  const more = total - shown.length;
  return (
    <span className="mt-3 block max-h-48 space-y-1 overflow-y-auto text-left">
      {shown.map((file) => {
        const kind = FILE_KIND[file.kind];
        return (
          <span
            key={file.path}
            className="flex items-baseline gap-2 font-mono text-xs"
          >
            <span
              className={`w-2.5 shrink-0 font-semibold ${kind.className}`}
              aria-label={kind.label}
              title={kind.label}
            >
              {kind.letter}
            </span>
            <span className="text-foreground min-w-0 break-all">
              {file.path}
            </span>
          </span>
        );
      })}
      {more > 0 && (
        <span className="block text-xs">
          and {more} more {more === 1 ? "file" : "files"}
        </span>
      )}
    </span>
  );
}

/** Other agents' turns a restore undoes too, named by engine and number. */
function AffectedTurnList({
  turns,
  undo,
}: {
  turns: readonly CodeRestoreAffectedTurn[];
  undo: boolean;
}) {
  const shown = turns.slice(0, LISTED_FILES);
  const more = turns.length - shown.length;
  return (
    <span className="mt-3 block text-left">
      {undo
        ? `It also undoes ${turns.length === 1 ? "the turn" : `the ${turns.length} turns`} made since the restore:`
        : `It also undoes ${turns.length === 1 ? "a turn" : `${turns.length} turns`} by other agents in this workspace:`}
      <span className="mt-1 block space-y-1">
        {shown.map((turn) => (
          <span key={turn.turn_id} className="text-foreground block text-xs">
            {HARNESS_LABELS[turn.harness_kind]}, turn {turn.ordinal}
          </span>
        ))}
        {more > 0 && (
          <span className="block text-xs">
            and {more} more {more === 1 ? "turn" : "turns"}
          </span>
        )}
      </span>
    </span>
  );
}

/** A bounded list of paths, one per line. */
function PathList({ paths }: { paths: readonly string[] }) {
  const shown = paths.slice(0, LISTED_FILES);
  const more = paths.length - shown.length;
  return (
    <span className="mt-3 block max-h-48 space-y-1 overflow-y-auto text-left">
      {shown.map((path) => (
        <span
          key={path}
          className="text-foreground block break-all font-mono text-xs"
        >
          {path}
        </span>
      ))}
      {more > 0 && (
        <span className="block text-xs">
          and {more} more {more === 1 ? "file" : "files"}
        </span>
      )}
    </span>
  );
}
