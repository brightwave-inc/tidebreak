/**
 * The provenance banner for a session an external channel created
 * (docs/slack-sessions.md, desktop pickup).
 *
 * The journal such a session shows is coarse on purpose — turns, per-turn
 * assistant records, artifacts, never tool activity. Without the banner a
 * thin journal reads as corruption; with it, as provenance. One line, one
 * link back to the thread, no further channel UI.
 */

import { MessageCircle } from "lucide-react";

import { openExternal } from "@/host";
import type { CodeSessionExternalOrigin } from "../generated/wire";

function channelLabel(kind: string): string {
  return kind === "slack" ? "Slack" : kind;
}

/**
 * The thread permalink for a Slack origin, when the key carries one.
 *
 * The adapter's durable key is `workspace/channel/thread_ts`. A key in any
 * other shape — a DM generation key, another channel family — yields no
 * link, and the banner renders without one rather than guessing.
 */
export function externalThreadUrl(
  origin: CodeSessionExternalOrigin,
): string | null {
  if (origin.channel_kind !== "slack") return null;
  const parts = origin.external_key.split("/");
  if (parts.length !== 3) return null;
  const [, channel, ts] = parts;
  if (!/^[A-Z0-9]+$/i.test(channel) || !/^\d+\.\d+$/.test(ts)) return null;
  return `https://slack.com/archives/${channel}/p${ts.replace(".", "")}`;
}

export function SessionOriginBanner({
  origin,
}: {
  origin: CodeSessionExternalOrigin;
}) {
  const url = externalThreadUrl(origin);
  return (
    <div
      className="border-border-subtle bg-background/85 mx-auto mt-3 flex w-[calc(100%-2rem)] max-w-3xl items-center gap-2 rounded-lg border px-3 py-2"
      data-testid="session-origin-banner"
    >
      <MessageCircle
        className="text-muted-foreground size-3.5 shrink-0"
        aria-hidden
      />
      <p className="text-muted-foreground min-w-0 flex-1 truncate text-xs">
        Started from {channelLabel(origin.channel_kind)}; engine activity stays
        in the sandbox.
      </p>
      {url && (
        <button
          type="button"
          className="text-foreground shrink-0 cursor-pointer text-xs underline underline-offset-2 hover:no-underline"
          onClick={() => void openExternal(url).catch(() => undefined)}
        >
          Open the thread
        </button>
      )}
    </div>
  );
}
