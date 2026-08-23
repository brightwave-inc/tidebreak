import { useEffect, useRef, useState, type ReactNode } from "react";
import { ExternalLink, RefreshCw } from "lucide-react";
import { toast } from "sonner";

import { HttpError, type ApiClient } from "../api/client";
import type { CodeWorkspacePrSnapshot } from "../api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ClipboardCopyButton } from "@/ClipboardCopyButton";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { Textarea } from "@/components/ui/textarea";
import { openExternal } from "@/host";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { FOCUS_RING } from "./interactive";
import {
  useCodeWorkspacePr,
  type CodeWorkspacePrMutation,
  type CodeWorkspacePrResource,
} from "./useCodeWorkspacePr";

/**
 * Commit, push, and pull-request card on a workspace.
 *
 * The default commit message is generated on the server from the worktree
 * diffstat and the workspace title. Creating a PR never merges it.
 *
 * Git state goes stale the moment the engine writes a file, so the card
 * refetches whenever the session's content revision moves, and offers a manual
 * refresh for edits Tidebreak never saw.
 */

export function PrCard({
  client,
  workspaceId,
  contentRevision = 0,
  framed = true,
}: {
  client: Pick<
    ApiClient,
    | "getCodeWorkspacePr"
    | "commitCodeWorkspace"
    | "pushCodeWorkspace"
    | "createCodePullRequest"
  >;
  workspaceId: string;
  /** Bumped by the session journal when the worktree may have moved. */
  contentRevision?: number;
  /** When false, drop the card chrome — the host already frames this. */
  framed?: boolean;
}) {
  const resource = useCodeWorkspacePr(client, workspaceId, contentRevision);
  return (
    <PrCardController
      client={client}
      workspaceId={workspaceId}
      framed={framed}
      resource={resource}
    />
  );
}

/** Use the page-level snapshot so the header and inspector never double-load. */
export function PrCardWithResource({
  client,
  workspaceId,
  framed = true,
  resource,
}: {
  client: Pick<
    ApiClient,
    "commitCodeWorkspace" | "pushCodeWorkspace" | "createCodePullRequest"
  >;
  workspaceId: string;
  framed?: boolean;
  resource: CodeWorkspacePrResource;
}) {
  return (
    <PrCardController
      client={client}
      workspaceId={workspaceId}
      framed={framed}
      resource={resource}
    />
  );
}

function PrCardController({
  client,
  workspaceId,
  framed,
  resource,
}: {
  client: Pick<
    ApiClient,
    "commitCodeWorkspace" | "pushCodeWorkspace" | "createCodePullRequest"
  >;
  workspaceId: string;
  framed: boolean;
  resource: CodeWorkspacePrResource;
}) {
  const [message, setMessage] = useState("");
  const lastSuggestedMessage = useRef<string | null>(null);

  const {
    data: snapshot,
    error: loadError,
    refreshing,
    refresh,
    adopt,
    busy,
    mutationError,
    setMutationError,
    runMutation,
  } = resource;

  // The server's suggestion is a live default, not an override: refresh it
  // while the box still contains the previous suggestion, but never clobber
  // what the operator has typed.
  useEffect(() => {
    if (!snapshot) return;
    const previousSuggestion = lastSuggestedMessage.current;
    const nextSuggestion = snapshot.suggested_commit_message;
    lastSuggestedMessage.current = nextSuggestion;
    setMessage((current) => {
      const untouched =
        current.trim().length === 0 || current === previousSuggestion;
      return untouched ? nextSuggestion : current;
    });
  }, [snapshot]);

  async function commit() {
    try {
      const committed = await runMutation("commit", async () => {
        await client.commitCodeWorkspace(workspaceId, message);
        await refresh();
        return true;
      });
      if (!committed) return;
      setMessage("");
      toast.success("Committed");
    } catch (err) {
      const message = friendlyErrorMessage(err, "Could not commit");
      setMutationError(message);
      toast.error(message);
    }
  }

  async function push() {
    try {
      const pushed = await runMutation("push", async () => {
        await client.pushCodeWorkspace(workspaceId);
        await refresh();
        return true;
      });
      if (!pushed) return;
      toast.success("Pushed");
    } catch (err) {
      const message =
        err instanceof HttpError && err.kind === "git_auth_failed"
          ? err.message
          : friendlyErrorMessage(err, "Could not push");
      setMutationError(message);
      toast.error(message);
    }
  }

  async function createPr() {
    try {
      const next = await runMutation("create_pr", async () => {
        const created = await client.createCodePullRequest(workspaceId);
        adopt(created);
        return created;
      });
      if (!next) return;
      const url = next.pr?.url;
      if (url && !(await openExternal(url).catch(() => false))) {
        toast.message("Copy the pull-request URL to open it.");
      }
      toast.success("Pull request created");
    } catch (err) {
      if (
        err instanceof HttpError &&
        (err.kind === "gh_absent" || err.kind === "gh_signed_out")
      ) {
        setMutationError(err.message);
      } else {
        const message = friendlyErrorMessage(
          err,
          "Could not create a pull request",
        );
        setMutationError(message);
        toast.error(message);
      }
    }
  }

  return (
    <PrCardView
      snapshot={snapshot}
      error={mutationError ?? loadError}
      message={message}
      busy={busy}
      refreshing={refreshing}
      framed={framed}
      onMessageChange={setMessage}
      onCommit={() => void commit()}
      onPush={() => void push()}
      onCreatePr={() => void createPr()}
      onRefresh={() => void refresh()}
    />
  );
}

export function PrCardView({
  snapshot,
  error,
  message,
  busy,
  refreshing = false,
  framed = true,
  onMessageChange,
  onCommit,
  onPush,
  onCreatePr,
  onRefresh,
}: {
  snapshot: CodeWorkspacePrSnapshot | null;
  error?: string | null;
  message: string;
  busy: CodeWorkspacePrMutation | null;
  refreshing?: boolean;
  framed?: boolean;
  onMessageChange: (value: string) => void;
  onCommit: () => void;
  onPush: () => void;
  onCreatePr: () => void;
  onRefresh?: () => void;
}) {
  const ghMissing = snapshot ? !snapshot.gh_found : false;
  const ghSignedOut = snapshot?.gh_authenticated === false;
  const canCommit = Boolean(snapshot?.dirty) && busy === null;
  const canPush = Boolean(snapshot?.unpushed) && busy === null;
  const canCreatePr =
    Boolean(snapshot) &&
    !snapshot?.dirty &&
    !snapshot?.unpushed &&
    snapshot?.ahead !== 0 &&
    !ghMissing &&
    !ghSignedOut &&
    !snapshot?.pr &&
    busy === null;
  const readyForPr =
    Boolean(snapshot) &&
    !snapshot?.dirty &&
    !snapshot?.unpushed &&
    snapshot?.ahead !== 0 &&
    !snapshot?.pr;

  return (
    <section
      className={
        framed
          ? "border-border bg-card flex flex-col gap-3 rounded-lg border px-3 py-3"
          : "flex flex-col gap-3"
      }
      aria-label="Git"
    >
      <header className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="text-sm font-medium">Git</h2>
        <div className="flex items-center gap-1">
          {snapshot ? (
            <GitStateChips snapshot={snapshot} />
          ) : (
            <Skeleton className="h-5 w-20" />
          )}
          {onRefresh && (
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label="Refresh git status"
              disabled={refreshing || busy !== null}
              onClick={onRefresh}
            >
              {refreshing ? <Spinner aria-hidden /> : <RefreshCw />}
            </Button>
          )}
        </div>
      </header>
      {error && (
        <p className="text-critical text-sm" role="alert">
          {error}
        </p>
      )}
      {snapshot?.pushes_as && (
        <p className="text-muted-foreground text-xs leading-relaxed">
          Pushes from this machine land as{" "}
          <span className="text-foreground font-medium">
            {snapshot.pushes_as}
          </span>
          , the deployment&apos;s GitHub App — not as your GitHub account.
        </p>
      )}
      {snapshot?.dirty ? (
        <div className="flex flex-col gap-2">
          <label className="flex flex-col gap-1">
            <span className="text-muted-foreground text-xs">
              Commit message
            </span>
            <Textarea
              rows={3}
              value={message}
              onChange={(event) => onMessageChange(event.target.value)}
              placeholder="Describe the change"
              disabled={busy !== null}
            />
          </label>
          <Button
            type="button"
            size="sm"
            className="self-start"
            disabled={!canCommit}
            onClick={onCommit}
          >
            {busy === "commit" && <Spinner aria-hidden />}
            {busy === "commit" ? "Committing…" : "Commit changes"}
          </Button>
        </div>
      ) : snapshot?.unpushed ? (
        <NextGitAction
          description={
            snapshot.pr
              ? `Push the latest commit to update #${snapshot.pr.number}.`
              : "The commit is ready locally. Push this branch to origin."
          }
          label={busy === "push" ? "Pushing…" : "Push branch"}
          busy={busy === "push"}
          disabled={!canPush}
          onClick={onPush}
        />
      ) : readyForPr ? (
        <NextGitAction
          description={
            ghMissing || ghSignedOut
              ? "The branch is pushed. Set up GitHub CLI to create its pull request."
              : "The branch is pushed and ready for a pull request."
          }
          label={busy === "create_pr" ? "Creating…" : "Create pull request"}
          busy={busy === "create_pr"}
          disabled={!canCreatePr}
          onClick={onCreatePr}
        />
      ) : snapshot?.pr ? (
        <PrDigest snapshot={snapshot} />
      ) : snapshot ? (
        <p className="text-muted-foreground text-xs leading-relaxed">
          No local commits yet. Changes made in this workspace appear here.
        </p>
      ) : null}
      {snapshot && readyForPr && (ghMissing || ghSignedOut) && (
        <GhRemediation snapshot={snapshot} />
      )}
    </section>
  );
}

function GitStateChips({ snapshot }: { snapshot: CodeWorkspacePrSnapshot }) {
  const chips: ReactNode[] = [];
  if (snapshot.pr) {
    chips.push(
      <Badge key="pr" variant={prStateVariant(snapshot.pr.state)} size="sm">
        {snapshot.pr.state}
      </Badge>,
    );
  }
  if (snapshot.dirty) {
    chips.push(
      <Badge key="dirty" variant="warning" size="sm">
        Uncommitted
      </Badge>,
    );
  } else if (snapshot.unpushed) {
    chips.push(
      <Badge key="unpushed" variant="warning" size="sm">
        Unpushed
      </Badge>,
    );
  } else if (!snapshot.pr && snapshot.ahead > 0) {
    chips.push(
      <Badge key="pushed" variant="info" size="sm">
        Pushed
      </Badge>,
    );
  } else if (!snapshot.pr) {
    chips.push(
      <Badge key="empty" variant="outline" size="sm">
        No commits
      </Badge>,
    );
  }
  return <div className="flex flex-wrap items-center gap-1">{chips}</div>;
}

function NextGitAction({
  description,
  label,
  busy,
  disabled,
  onClick,
}: {
  description: string;
  label: string;
  busy: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <div className="flex flex-col items-start gap-2">
      <p className="text-muted-foreground text-xs leading-relaxed">
        {description}
      </p>
      <Button type="button" size="sm" disabled={disabled} onClick={onClick}>
        {busy && <Spinner aria-hidden />}
        {label}
      </Button>
    </div>
  );
}

function PrDigest({ snapshot }: { snapshot: CodeWorkspacePrSnapshot }) {
  const pr = snapshot.pr;
  if (!pr) return null;
  return (
    <div className="flex items-center justify-between gap-2 text-xs">
      <p className="min-w-0 truncate">Pull request #{pr.number}</p>
      {pr.url && (
        <a
          href={pr.url}
          className={cn(
            "text-info-foreground flex shrink-0 items-center gap-1 rounded-sm underline-offset-2 hover:underline",
            FOCUS_RING,
          )}
          onClick={(event) => {
            event.preventDefault();
            void openExternal(pr.url!).catch(() => undefined);
          }}
        >
          Open
          <ExternalLink className="size-3" aria-hidden />
        </a>
      )}
    </div>
  );
}

function GhRemediation({ snapshot }: { snapshot: CodeWorkspacePrSnapshot }) {
  return (
    <div className="border-warning-border bg-warning-background text-warning-foreground flex flex-col gap-2 rounded-md border px-3 py-2 text-xs">
      <p>
        {snapshot.gh_found
          ? "gh is installed but not signed in."
          : "gh is not installed."}{" "}
        Copy the commands below into a terminal. Tidebreak does not store GitHub
        credentials.
      </p>
      <pre className="bg-background text-foreground overflow-x-auto rounded-md p-2 font-mono whitespace-pre-wrap">
        {snapshot.remediation}
      </pre>
      <ClipboardCopyButton
        value={snapshot.remediation}
        label="Copy instructions"
        copiedAnnouncement="Copied pull-request instructions"
        failedAnnouncement="Could not copy instructions"
      />
    </div>
  );
}

type StatusTone = "success" | "warning" | "critical" | "info" | "outline";

function prStateVariant(state: string): StatusTone {
  const token = state.toLowerCase();
  if (token === "open") return "success";
  if (token === "merged") return "info";
  if (token === "closed") return "critical";
  return "outline";
}
