import { useState, type FormEvent, type KeyboardEvent } from "react";
import { GitCommitHorizontal } from "lucide-react";
import { toast } from "sonner";

import { HttpError } from "../api/client";
import type { CodeCommitSnapshot } from "../api/types";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Textarea } from "@/components/ui/textarea";
import { friendlyErrorMessage } from "@/lib/utils";

/**
 * Commit every change in the worktree with a message, from Source control.
 *
 * The box commits what the worktree holds, the way `git add -A && git commit`
 * would: there is no staging area to manage. An empty message uses the
 * suggested one. A refused commit, most often a hook, stays on screen with
 * what git printed, because that output is how the person learns what to fix.
 */
export function CommitBox({
  dirty,
  changedFiles,
  suggestedMessage,
  busy = false,
  unavailableReason,
  onCommit,
}: {
  /** Whether the worktree holds uncommitted work; `null` while unknown. */
  dirty: boolean | null;
  /** How many listed files a commit would carry, when the list says. */
  changedFiles?: number;
  /** What the server writes when the message is empty. */
  suggestedMessage?: string;
  /** Another Git action on this workspace is running. */
  busy?: boolean;
  /** Why nothing can be committed right now, such as a running turn. */
  unavailableReason?: string;
  onCommit: (message: string | undefined) => Promise<CodeCommitSnapshot>;
}) {
  const [message, setMessage] = useState("");
  const [committing, setCommitting] = useState(false);
  const [failure, setFailure] = useState<CommitFailure | null>(null);
  const suggested = firstLine(suggestedMessage);
  const placeholder = suggested
    ? `Message. Leave empty to use “${suggested}”.`
    : "Commit message";
  const disabled =
    committing || busy || dirty !== true || unavailableReason !== undefined;

  async function commit() {
    if (disabled) return;
    setCommitting(true);
    setFailure(null);
    try {
      const committed = await onCommit(message.trim() || undefined);
      setMessage("");
      toast.success(`Committed ${committed.sha.slice(0, 7)}`);
    } catch (error) {
      setFailure(commitFailure(error));
    } finally {
      setCommitting(false);
    }
  }

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    void commit();
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void commit();
    }
  }

  return (
    <form
      className="flex shrink-0 flex-col gap-2 border-b px-3 pt-3 pb-3"
      aria-label="Commit"
      onSubmit={onSubmit}
    >
      <Textarea
        value={message}
        rows={2}
        placeholder={placeholder}
        aria-label="Commit message"
        className="min-h-14 resize-none text-sm"
        disabled={committing}
        onChange={(event) => {
          setMessage(event.target.value);
          if (failure) setFailure(null);
        }}
        onKeyDown={onKeyDown}
      />
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <Button type="submit" size="sm" disabled={disabled}>
          {committing ? (
            <Spinner className="size-3.5" aria-hidden="true" />
          ) : (
            <GitCommitHorizontal aria-hidden="true" />
          )}
          {committing ? "Committing…" : "Commit"}
        </Button>
        <p className="text-muted-foreground min-w-0 flex-1 text-xs">
          {commitHint({ dirty, changedFiles, unavailableReason })}
        </p>
      </div>
      {failure && (
        <div
          role="alert"
          className="notice-surface notice-critical rounded-md border px-3 py-2 text-sm"
        >
          <p className="font-medium">{failure.title}</p>
          {failure.detail && (
            <pre className="mt-1 max-h-40 overflow-auto font-mono text-xs whitespace-pre-wrap break-words">
              {failure.detail}
            </pre>
          )}
        </div>
      )}
    </form>
  );
}

type CommitFailure = { title: string; detail?: string };

/**
 * Split a refused commit into the sentence and what git printed.
 *
 * The server writes the sentence, a blank line, then git's output, so the
 * output keeps its own line breaks in a block the person can read.
 */
export function commitFailure(error: unknown): CommitFailure {
  const message = friendlyErrorMessage(error, "Could not commit");
  if (error instanceof HttpError && error.kind === "nothing_to_commit") {
    return { title: "There is nothing to commit." };
  }
  const split = message.indexOf("\n\n");
  if (split === -1) return { title: message };
  return {
    title: message.slice(0, split),
    detail: message.slice(split + 2),
  };
}

function commitHint({
  dirty,
  changedFiles,
  unavailableReason,
}: {
  dirty: boolean | null;
  changedFiles?: number;
  unavailableReason?: string;
}): string {
  if (dirty === false) return "No changes to commit.";
  if (unavailableReason) return unavailableReason;
  if (dirty === null) return "Reading the worktree…";
  if (changedFiles !== undefined && changedFiles > 0) {
    return `Commits ${changedFiles} changed ${changedFiles === 1 ? "file" : "files"}.`;
  }
  return "Commits every change.";
}

function firstLine(text?: string): string | undefined {
  const line = text?.split("\n")[0]?.trim();
  return line ? line : undefined;
}
