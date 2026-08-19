import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";
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
import { friendlyErrorMessage } from "@/lib/utils";
import { useLiveResource } from "./useLiveContent";

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
  const [message, setMessage] = useState("");
  const lastSuggestedMessage = useRef<string | null>(null);
  const [busy, setBusy] = useState<"commit" | "push" | "pr" | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const load = useCallback(
    () => client.getCodeWorkspacePr(workspaceId),
    [client, workspaceId],
  );
  const {
    data: snapshot,
    error: loadError,
    refreshing,
    refresh,
    adopt,
  } = useLiveResource({
    key: workspaceId,
    revision: contentRevision,
    load,
    errorMessage: "Could not load pull-request status",
  });

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
    setBusy("commit");
    try {
      await client.commitCodeWorkspace(workspaceId, message);
      setMessage("");
      setActionError(null);
      await refresh();
      toast.success("Committed");
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not commit"));
    } finally {
      setBusy(null);
    }
  }

  async function push() {
    setBusy("push");
    try {
      await client.pushCodeWorkspace(workspaceId);
      setActionError(null);
      await refresh();
      toast.success("Pushed");
    } catch (err) {
      if (err instanceof HttpError && err.kind === "git_auth_failed") {
        toast.error(err.message);
      } else {
        toast.error(friendlyErrorMessage(err, "Could not push"));
      }
    } finally {
      setBusy(null);
    }
  }

  async function createPr() {
    setBusy("pr");
    try {
      const next = await client.createCodePullRequest(workspaceId);
      setActionError(null);
      adopt(next);
      const url = next.pr?.url;
      if (url && !(await openExternal(url).catch(() => false))) {
        toast.message("Copy the pull-request URL to open it.");
      }
      toast.success("Pull request created");
    } catch (err) {
      if (err instanceof HttpError && (err.kind === "gh_absent" || err.kind === "gh_signed_out")) {
        setActionError(err.message);
      } else {
        toast.error(friendlyErrorMessage(err, "Could not create a pull request"));
      }
    } finally {
      setBusy(null);
    }
  }

  return (
    <PrCardView
      snapshot={snapshot}
      error={actionError ?? loadError}
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
  busy: "commit" | "push" | "pr" | null;
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
      <label className="flex flex-col gap-1">
        <span className="text-muted-foreground text-xs">Commit message</span>
        <Textarea
          rows={3}
          value={message}
          onChange={(event) => onMessageChange(event.target.value)}
          placeholder="Describe the change"
          disabled={busy !== null}
        />
      </label>
      <div className="flex flex-wrap items-center gap-2">
        <Button
          type="button"
          size="sm"
          disabled={!canCommit}
          onClick={onCommit}
        >
          Commit
        </Button>
        <Button
          type="button"
          size="sm"
          variant="secondary"
          disabled={!canPush}
          onClick={onPush}
        >
          Push
        </Button>
        <Button
          type="button"
          size="sm"
          variant="secondary"
          disabled={!canCreatePr}
          onClick={onCreatePr}
        >
          Create PR
        </Button>
      </div>
      {snapshot && (ghMissing || ghSignedOut) && (
        <GhRemediation snapshot={snapshot} />
      )}
      {snapshot?.pr && <PrDigest snapshot={snapshot} />}
    </section>
  );
}

function GitStateChips({ snapshot }: { snapshot: CodeWorkspacePrSnapshot }) {
  if (snapshot.pr) {
    return (
      <div className="flex flex-wrap items-center gap-1">
        <Badge variant={prStateVariant(snapshot.pr.state)} size="sm">
          {snapshot.pr.state}
        </Badge>
      </div>
    );
  }
  if (snapshot.dirty) {
    return (
      <Badge variant="warning" size="sm">
        Uncommitted
      </Badge>
    );
  }
  if (snapshot.unpushed) {
    return (
      <Badge variant="warning" size="sm">
        Unpushed
      </Badge>
    );
  }
  if (snapshot.ahead > 0) {
    return (
      <Badge variant="info" size="sm">
        Pushed
      </Badge>
    );
  }
  return (
    <Badge variant="outline" size="sm">
      No commits
    </Badge>
  );
}

function PrDigest({ snapshot }: { snapshot: CodeWorkspacePrSnapshot }) {
  const pr = snapshot.pr;
  if (!pr) return null;
  return (
    <div className="flex flex-col gap-1 text-xs">
      <p>
        #{pr.number}
        {pr.url ? (
          <>
            {" "}
            <a
              href={pr.url}
              className="text-info-foreground underline"
              onClick={(event) => {
                event.preventDefault();
                void openExternal(pr.url!).catch(() => undefined);
              }}
            >
              {pr.url}
            </a>
          </>
        ) : null}
      </p>
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

