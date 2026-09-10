import { useEffect, useRef, useState } from "react";
import { useParams } from "@tanstack/react-router";

import { useApp } from "./AppContext";
import { type CodeConnectPage } from "./api";
import { connectPageFailurePhase, channelLabel } from "./ConnectApprovalRoute";
import { Logomark } from "./Logomark";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";

type WorkspaceApprovalPhase =
  | "loading"
  | "ready"
  | "approving"
  | "approved"
  | "invalid"
  | "unavailable";

/**
 * The admin approval for a workspace grant. The adapter starts the
 * handshake; an admin says the shared identity may run channel sessions
 * for that Slack workspace.
 */
export function WorkspaceApprovalRoute() {
  const { id } = useParams({ strict: false }) as { id: string };
  const { client } = useApp();
  const [page, setPage] = useState<CodeConnectPage | null>(null);
  const [phase, setPhase] = useState<WorkspaceApprovalPhase>("loading");
  const [error, setError] = useState<string | null>(null);
  const [loadAttempt, setLoadAttempt] = useState(0);
  const approvalVersion = useRef(0);

  useEffect(() => {
    let cancelled = false;
    setPage(null);
    setError(null);
    setPhase("loading");
    void (async () => {
      try {
        const next = await client.getWorkspaceGrantPage(id);
        if (cancelled) return;
        setPage(next);
        setPhase(next.state === "approved" ? "approved" : "ready");
      } catch (loadError) {
        if (!cancelled) setPhase(connectPageFailurePhase(loadError));
      }
    })();
    return () => {
      cancelled = true;
      approvalVersion.current += 1;
    };
  }, [client, loadAttempt, id]);

  async function approve() {
    if (!page) return;
    const version = approvalVersion.current;
    setPhase("approving");
    setError(null);
    try {
      await client.approveWorkspaceGrant(id, page.csrf);
      if (approvalVersion.current !== version) return;
      setPhase("approved");
    } catch {
      if (approvalVersion.current !== version) return;
      setError("The workspace grant could not be approved. Try again.");
      setPhase("ready");
    }
  }

  return (
    <WorkspaceApprovalView
      page={page}
      phase={phase}
      error={error}
      onApprove={() => void approve()}
      onRetry={() => setLoadAttempt((attempt) => attempt + 1)}
    />
  );
}

export function WorkspaceApprovalView({
  page,
  phase,
  error,
  onApprove,
  onRetry,
}: {
  page: CodeConnectPage | null;
  phase: WorkspaceApprovalPhase;
  error: string | null;
  onApprove: () => void;
  onRetry: () => void;
}) {
  return (
    <div className="boot" aria-label="Workspace grant approval">
      <div className="boot-brand">
        <Logomark />
        <h1>Tidebreak</h1>
      </div>
      <Card className="w-full max-w-md p-6 text-left">
        {phase === "loading" ? (
          <p className="text-sm text-muted-foreground" role="status">
            Opening the workspace grant…
          </p>
        ) : phase === "invalid" ? (
          <div className="flex flex-col gap-2">
            <h1 className="text-lg font-semibold">
              This workspace grant is no longer valid
            </h1>
            <p className="text-sm text-muted-foreground">
              It may have expired or already been used. Start again from the
              adapter.
            </p>
          </div>
        ) : phase === "unavailable" ? (
          <div className="flex flex-col items-start gap-3" role="alert">
            <div className="flex flex-col gap-2">
              <h1 className="text-lg font-semibold">
                The workspace grant could not be opened
              </h1>
              <p className="text-sm text-muted-foreground">
                The link may still be valid. Check that Tidebreak is running,
                then try again before the request expires.
              </p>
            </div>
            <Button type="button" variant="outline" onClick={onRetry}>
              Try again
            </Button>
          </div>
        ) : page ? (
          <div className="flex flex-col gap-4">
            <div className="min-w-0">
              <h1 className="text-lg font-semibold">{page.workspace_name}</h1>
              <p className="text-sm text-muted-foreground">
                {channelLabel(page.channel_kind)} workspace
              </p>
            </div>
            {phase === "approved" ? (
              <p className="text-sm leading-relaxed">
                Approved. Return to the adapter so it can finish connecting.
                Channel sessions for this workspace will run as{" "}
                {page.display_name}.
              </p>
            ) : (
              <>
                <p className="text-sm leading-relaxed">
                  Run channel sessions for {channelLabel(page.channel_kind)}{" "}
                  workspace {page.workspace_name} as {page.display_name}? Every
                  channel can use the repositories available to this instance’s
                  GitHub App. Agents can choose repositories and work across
                  them.
                </p>
                <div className="flex gap-2">
                  <Button
                    type="button"
                    disabled={phase === "approving"}
                    onClick={onApprove}
                  >
                    {phase === "approving" ? "Approving…" : "Approve workspace"}
                  </Button>
                </div>
              </>
            )}
            {error && (
              <p className="text-sm text-destructive" role="alert">
                {error}
              </p>
            )}
          </div>
        ) : null}
      </Card>
    </div>
  );
}
