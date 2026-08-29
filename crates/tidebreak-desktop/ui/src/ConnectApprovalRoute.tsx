import { useEffect, useState } from "react";
import { useParams } from "@tanstack/react-router";

import { useApp } from "./AppContext";
import type { CodeConnectPage } from "./api";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";

/**
 * The connect approval a channel's connect card links to
 * (docs/slack-sessions.md, stage 2).
 *
 * Shows exactly the identity being linked — the channel workspace, the
 * display name, the avatar — and asks "is this you?". Approving mints
 * nothing by itself: the adapter's closing confirm in the channel does,
 * so a forwarded link binds nothing. A used or expired link renders its
 * refusal instead of a form.
 */
export function ConnectApprovalRoute() {
  const { nonce } = useParams({ strict: false }) as { nonce: string };
  const { client } = useApp();
  const [page, setPage] = useState<CodeConnectPage | null>(null);
  const [phase, setPhase] = useState<
    "loading" | "ready" | "approving" | "approved" | "invalid"
  >("loading");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const next = await client.getCodeConnectPage(nonce);
        if (cancelled) return;
        setPage(next);
        setPhase(next.state === "approved" ? "approved" : "ready");
      } catch {
        if (!cancelled) setPhase("invalid");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, nonce]);

  async function approve() {
    if (!page) return;
    setPhase("approving");
    setError(null);
    try {
      await client.approveCodeConnect(nonce, page.csrf);
      setPhase("approved");
    } catch {
      setError("The connect request could not be approved. Try again.");
      setPhase("ready");
    }
  }

  return (
    <ConnectApprovalView
      page={page}
      phase={phase}
      error={error}
      onApprove={() => void approve()}
    />
  );
}

/**
 * The page itself, pure so Storybook can show every state without a
 * router or a live client.
 */
export function ConnectApprovalView({
  page,
  phase,
  error,
  onApprove,
}: {
  page: CodeConnectPage | null;
  phase: "loading" | "ready" | "approving" | "approved" | "invalid";
  error: string | null;
  onApprove: () => void;
}) {
  return (
    <div className="flex min-h-full items-center justify-center p-6">
      <Card className="w-full max-w-md p-6">
        {phase === "loading" ? (
          <p className="text-sm text-muted-foreground" role="status">
            Opening the connect request…
          </p>
        ) : phase === "invalid" ? (
          <div className="flex flex-col gap-2">
            <h1 className="text-lg font-semibold">
              This connect link is no longer valid
            </h1>
            <p className="text-sm text-muted-foreground">
              It may have expired or already been used. Start again from the
              channel: mention the agent and follow the fresh link it posts.
            </p>
          </div>
        ) : page ? (
          <div className="flex flex-col gap-4">
            <div className="flex items-center gap-3">
              <ConnectAvatar page={page} />
              <div className="min-w-0">
                <h1 className="text-lg font-semibold">{page.display_name}</h1>
                <p className="text-sm text-muted-foreground">
                  {channelLabel(page.channel_kind)} · {page.workspace_name}
                </p>
              </div>
            </div>
            {phase === "approved" ? (
              <p className="text-sm leading-relaxed">
                Approved. To finish connecting, return to{" "}
                {channelLabel(page.channel_kind)} and confirm there — the direct
                message proves the account is yours, and nothing is linked until
                it lands.
              </p>
            ) : (
              <>
                <p className="text-sm leading-relaxed">
                  Is this you? Approving lets this{" "}
                  {channelLabel(page.channel_kind)} account start and steer
                  coding sessions on your machine. If you did not ask to
                  connect, close this page.
                </p>
                <div className="flex gap-2">
                  <Button
                    type="button"
                    disabled={phase === "approving"}
                    onClick={onApprove}
                  >
                    {phase === "approving" ? "Approving…" : "Yes, this is me"}
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

function ConnectAvatar({ page }: { page: CodeConnectPage }) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [page.avatar_url]);
  if (!page.avatar_url || failed) {
    return (
      <div
        aria-hidden
        className="flex size-12 shrink-0 items-center justify-center rounded-full bg-muted text-lg font-medium"
      >
        {page.display_name.slice(0, 1).toUpperCase()}
      </div>
    );
  }
  return (
    <img
      src={page.avatar_url}
      alt={`${page.display_name}'s avatar`}
      className="size-12 shrink-0 rounded-full object-cover"
      referrerPolicy="no-referrer"
      decoding="async"
      onError={() => setFailed(true)}
    />
  );
}

function channelLabel(kind: string): string {
  return kind === "slack" ? "Slack" : kind;
}
