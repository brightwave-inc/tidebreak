/** Identifies the channel that started the session and links back to its thread. */

import { MessageCircle } from "lucide-react";

import { openExternal } from "@/host";
import type {
  ExecutionLocation,
  SessionExternalOrigin as CodeSessionExternalOrigin,
} from "../generated/wire";

function channelLabel(kind: string): string {
  return kind === "slack" ? "Slack" : kind;
}

/**
 * Slack keys carry `workspace:channel:thread_ts`. Preserve links for legacy
 * slash-delimited keys; DM generations have no thread timestamp.
 */
export function externalThreadUrl(
  origin: CodeSessionExternalOrigin,
): string | null {
  if (origin.channel_kind !== "slack") return null;
  const parts = origin.external_key.split(
    origin.external_key.includes(":") ? ":" : "/",
  );
  if (parts.length !== 3) return null;
  const [, channel, ts] = parts;
  if (!/^[A-Z0-9]+$/i.test(channel) || !/^\d+\.\d+$/.test(ts)) return null;
  return `https://slack.com/archives/${channel}/p${ts.replace(".", "")}`;
}

export function SessionOriginBanner({
  origin,
  executionLocation,
}: {
  origin: CodeSessionExternalOrigin;
  executionLocation: ExecutionLocation;
}) {
  const url = externalThreadUrl(origin);
  return (
    <div
      className="border-border-subtle bg-background/85 mx-auto mt-3 flex w-[calc(100%-2rem)] max-w-3xl items-start gap-2 rounded-lg border px-3 py-2"
      data-testid="session-origin-banner"
    >
      <MessageCircle
        className="text-muted-foreground mt-px size-3.5 shrink-0"
        aria-hidden
      />
      <p className="text-muted-foreground min-w-0 flex-1 text-xs">
        Started from {channelLabel(origin.channel_kind)}; runs{" "}
        {executionLocation === "machine" ? "on this machine" : "in a sandbox"}.
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
