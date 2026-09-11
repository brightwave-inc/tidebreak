import { toast } from "sonner";
import { useSessionDigest } from "./CodeUpdatesStore";
import { SessionRecoveryNotice } from "./SessionRecoveryNotice";
import { sessionRecoveryState } from "./sessionRecovery";
import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useApp } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type { CodeSessionSnapshot, ModelInfo } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { friendlyErrorMessage } from "@/lib/utils";
import { RouteFrame } from "@/RouteFrame";
import { codeClientGeneration } from "./CodeClientGeneration";
import { CodeSidebar } from "./CodeSidebar";
import { SessionLifecycleIndicator } from "./SessionLifecycleIndicator";
import { CodeSessionPane } from "./workspace/CodeSessionPane";

/** Resolve a durable conversation link against the machine serving this page. */
export function CodeSessionPage({ sessionId }: { sessionId: string }) {
  const { client } = useApp();
  return (
    <CodeSessionRouteBody
      key={`${codeClientGeneration(client)}:${sessionId}`}
      sessionId={sessionId}
    />
  );
}

function CodeSessionRouteBody({ sessionId }: { sessionId: string }) {
  const { client, models, defaultModelKey } = useApp();
  const navigate = useNavigate();
  const [session, setSession] = useState<CodeSessionSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let cancelled = false;
    setSession(null);
    setError(null);
    void client
      .getCodeSession(sessionId)
      .then((next) => {
        if (cancelled) return;
        if (next.workspace_id) {
          void navigate({
            to: "/code/w/$workspaceId",
            params: { workspaceId: next.workspace_id },
            search: { task: next.id },
            replace: true,
          });
        } else {
          setSession(next);
        }
      })
      .catch((err: unknown) => {
        if (!cancelled)
          setError(
            friendlyErrorMessage(err, "Could not open this conversation"),
          );
      });
    return () => {
      cancelled = true;
    };
  }, [client, sessionId, navigate, attempt]);
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <CodeSessionContent
        key={sessionId}
        session={session}
        error={error}
        client={client}
        models={models}
        defaultModelKey={defaultModelKey}
        onRetry={() => setAttempt((value) => value + 1)}
        onRecovered={setSession}
      />
    </RouteFrame>
  );
}

/** The session transcript also serves conversations that have no workspace. */
export function CodeSessionContent({
  title = "Conversation",
  session,
  error,
  client,
  models,
  defaultModelKey,
  onRetry,
  onRecovered,
}: {
  title?: string;
  session: CodeSessionSnapshot | null;
  error: string | null;
  client: ApiClient;
  models: ModelInfo[];
  defaultModelKey: string | null;
  onRetry: () => void;
  onRecovered?: (session: CodeSessionSnapshot) => void;
}) {
  const digest = useSessionDigest(undefined, session?.id ?? null);
  const recovery = sessionRecoveryState(session, digest);
  const [retrying, setRetrying] = useState(false);
  async function retryRecovery() {
    if (!session || retrying) return;
    setRetrying(true);
    try {
      const recovered = await client.reapCodeSession(session.id);
      onRecovered?.(recovered);
    } catch (error) {
      toast.error(friendlyErrorMessage(error, "Could not recover the session"));
    } finally {
      setRetrying(false);
    }
  }
  return (
    <div className="content-container flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden">
      <div className="flex h-12 shrink-0 items-center gap-3 border-b border-border px-4">
        <h1 className="min-w-0 flex-1 truncate text-sm font-medium">{title}</h1>
        {session && (
          <SessionLifecycleIndicator
            lifecycle={recovery.lifecycle ?? session.lifecycle}
            attention={recovery.attention}
            harness={session.harness_kind}
            unrecognizedEventCount={session.unrecognized_event_count}
          />
        )}
      </div>
      {error ? (
        <div
          role="alert"
          className="flex flex-1 flex-col items-center justify-center gap-3 p-6"
        >
          <p className="text-muted-foreground max-w-sm text-center text-sm">
            {error}
          </p>
          <Button size="sm" onClick={onRetry}>
            Retry
          </Button>
        </div>
      ) : session ? (
        <>
          <SessionRecoveryNotice
            showProgress={false}
            lifecycle={recovery.lifecycle}
            attention={recovery.attention}
            reason={recovery.reason}
            retrying={retrying}
            allowRetry={session.access !== "view"}
            onRetry={() => void retryRecovery()}
          />
          <CodeSessionPane
            key={session.id}
            session={session}
            client={client}
            catalogModels={models}
            defaultModelKey={defaultModelKey}
            disabled={recovery.blocksTurn}
          />
        </>
      ) : (
        <div
          role="status"
          aria-label="Opening conversation"
          className="flex flex-1 flex-col gap-4 p-6"
        >
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-4 w-3/4" />
        </div>
      )}
    </div>
  );
}
