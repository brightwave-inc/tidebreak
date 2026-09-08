/** Identifies the channel that started the session and links back to its thread. */

import { MessageCircle } from "lucide-react";

import { openExternal } from "@/host";
import type {
  ExecutionLocation,
  SessionExternalOrigin as CodeSessionExternalOrigin,
} from "../generated/wire";

export function channelLabel(kind: string): string {
  return kind === "slack" ? "Slack" : kind;
}

/** Slack conversation id from a colon- or slash-delimited external key. */
export function slackChannelId(externalKey: string): string {
  const parts = externalKey.split(externalKey.includes(":") ? ":" : "/");
  return parts[1] ?? "";
}

/**
 * Slack DM ids start with `D`. Everything else is a channel (including
 * private groups).
 */
export function slackConversationKind(externalKey: string): "dm" | "channel" {
  return /^D/i.test(slackChannelId(externalKey)) ? "dm" : "channel";
}

/** Rail group name: channel family plus DM or channel. */
export function sessionOriginGroupLabel(
  origin: CodeSessionExternalOrigin,
): string {
  const name = channelLabel(origin.channel_kind);
  return slackConversationKind(origin.external_key) === "dm"
    ? `${name} DM`
    : `${name} channel`;
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
  origins,
  executionLocation,
  actsAs,
}: {
  origin: CodeSessionExternalOrigin;
  origins?: CodeSessionExternalOrigin[];
  executionLocation: ExecutionLocation;
  actsAs?: "person" | "bot";
}) {
  const conversations = origins?.length ? origins : [origin];
  const links = conversations.flatMap((conversation, index) => {
    const url = externalThreadUrl(conversation);
    return url ? [{ url, index }] : [];
  });
  const where =
    executionLocation === "machine" ? "on this machine" : "in a sandbox";
  const who =
    actsAs === "bot" ? " as the bot" : actsAs === "person" ? " as you" : "";
  return (
    <div
      className="border-border-subtle bg-background/85 mx-auto mt-3 flex w-[calc(100%-2rem)] max-w-3xl items-start gap-2 rounded-lg border px-3 py-2"
      data-testid="session-origin-banner"
    >
      <MessageCircle
        className="text-muted-foreground mt-px size-3.5 shrink-0"
        aria-hidden
      />
      <div className="flex min-w-0 flex-1 flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
        <p className="text-muted-foreground text-xs">
          Started from {channelLabel(origin.channel_kind)}; runs {where}
          {who}.
          {conversations.length > 1 &&
            ` Shared across ${conversations.length} conversations.`}
        </p>
        {links.length > 0 && (
          <div className="flex flex-wrap gap-x-3 gap-y-1">
            {links.map(({ url, index }) => (
              <button
                key={url}
                type="button"
                className="text-foreground cursor-pointer text-xs underline underline-offset-2 hover:no-underline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
                onClick={() => void openExternal(url).catch(() => undefined)}
              >
                {conversations.length === 1
                  ? "Open the thread"
                  : `Open thread ${index + 1}`}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
