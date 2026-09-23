import { CircleAlert } from "lucide-react";

import type { CodeProjectConfigFile } from "@/api/types";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Spinner } from "@/components/ui/spinner";
import { projectConfigEngines, projectConfigSummary } from "./repositoryTrust";

/** What the reader chose on the trust sheet. */
export type RepositoryTrustChoice = "trust" | "continue";

/**
 * The engine config a checkout carries, one row per file: its path, what
 * loading it would do, and which engines read it. A long list scrolls, and
 * its height leaves part of the next row showing so the scroll is visible.
 */
export function ProjectConfigFileList({
  files,
}: {
  files: readonly CodeProjectConfigFile[];
}) {
  return (
    <ul
      aria-label="Engine settings in this repository"
      className="max-h-72 divide-y divide-border-subtle overflow-auto rounded-md border border-border"
    >
      {files.map((file) => (
        <li key={file.path} className="flex min-w-0 flex-col gap-0.5 px-3 py-2">
          <span className="font-mono text-sm break-all text-foreground">
            {file.path}
          </span>
          <span className="text-sm text-muted-foreground">
            {projectConfigSummary(file)}
            <span aria-hidden="true"> · </span>
            <span className="sr-only">, read by </span>
            {projectConfigEngines(file)}
          </span>
        </li>
      ))}
    </ul>
  );
}

/**
 * The question a repository's own engine config raises before its first
 * session: load it, or start without it.
 *
 * A repository can carry settings that run commands on the reader's computer
 * as soon as an engine starts, outside Tidebreak's approvals. The sheet lists
 * each file and what it would do, and records the answer for the repository.
 * "Continue without its settings" takes focus first because it is the safe
 * answer, and Escape closes the sheet with the same outcome for this session
 * only.
 */
export function RepositoryTrustSheet({
  open,
  repoLabel,
  files,
  saving = null,
  error = null,
  onChoose,
  onDismiss,
}: {
  open: boolean;
  /** How the rail names the repository, when the workspace carries it. */
  repoLabel?: string | null;
  files: readonly CodeProjectConfigFile[];
  /** The answer being recorded, while its request is in flight. */
  saving?: RepositoryTrustChoice | null;
  /** Why the last answer could not be recorded. */
  error?: string | null;
  onChoose: (choice: RepositoryTrustChoice) => void;
  /** The sheet closed without an answer; the session starts without them. */
  onDismiss: () => void;
}) {
  const busy = saving !== null;
  const subject = repoLabel ? (
    <span className="font-medium text-foreground">{repoLabel}</span>
  ) : (
    "This repository"
  );
  return (
    <AlertDialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !busy) onDismiss();
      }}
    >
      <AlertDialogContent className="max-w-lg">
        {/* Left-aligned at every width, so the copy lines up with the list. */}
        <AlertDialogHeader className="text-left">
          <AlertDialogTitle>Trust this repository?</AlertDialogTitle>
          <AlertDialogDescription>
            {subject} has its own settings for coding engines. They can run
            commands on your computer as soon as a session starts, outside
            Tidebreak's approvals.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <ProjectConfigFileList files={files} />
        <p className="text-sm text-muted-foreground">
          Trust it only if you trust everyone who can change it. You can change
          this later in the repository's settings.
        </p>
        {error && (
          <div
            role="alert"
            className="notice-surface notice-critical flex items-start gap-2 rounded-md border px-3 py-2 text-sm"
          >
            <CircleAlert
              aria-hidden="true"
              className="mt-0.5 size-3.5 shrink-0"
            />
            <span className="min-w-0 break-words">{error}</span>
          </div>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel
            disabled={busy}
            onClick={(event) => {
              event.preventDefault();
              onChoose("continue");
            }}
          >
            {saving === "continue" && (
              <Spinner aria-hidden="true" className="size-3.5" />
            )}
            Continue without its settings
          </AlertDialogCancel>
          <AlertDialogAction
            disabled={busy}
            onClick={(event) => {
              event.preventDefault();
              onChoose("trust");
            }}
          >
            {saving === "trust" && (
              <Spinner aria-hidden="true" className="size-3.5 text-current" />
            )}
            Trust this repository
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
