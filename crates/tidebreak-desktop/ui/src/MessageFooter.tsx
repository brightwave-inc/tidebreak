import type { RefObject } from "react";
import { WithTooltip } from "@/components/ui/tooltip";
import {
  ClipboardCopyButton,
  copyPlainText,
  copyRichText,
  type ClipboardWriter,
} from "./ClipboardCopyButton";

type MessageFooterProps = {
  role: "user" | "assistant";
  text: string;
  createdAt?: string;
  settled?: boolean;
  /** Rendered assistant Markdown, read only when Copy is clicked. */
  richContentRef?: RefObject<HTMLElement | null>;
  /** A turn's assistant prose is split into one bubble per activity phase;
   *  the copy action and timestamp belong to the turn, so only the bubble
   *  that closes it carries them. */
  sequenceEnd?: boolean;
};

export function MessageFooter({
  role,
  text,
  createdAt,
  settled = true,
  richContentRef,
  sequenceEnd = true,
}: MessageFooterProps) {
  if (role === "assistant" && !sequenceEnd) return null;
  const hasContent = text.trim().length > 0;
  const timestamp = createdAt && (role === "user" || (settled && hasContent))
    ? formatMessageTimestamp(createdAt, new Date())
    : null;
  const canCopy = role === "assistant" && settled && hasContent;

  if (!canCopy && !timestamp) return null;

  return (
    <footer className="message-footer">
      {canCopy && (
        <ClipboardCopyButton
          copy={() =>
            copyMessageContent(richContentRef?.current?.innerHTML ?? "", text)
          }
          label="Copy"
          copiedAnnouncement="Message copied to clipboard."
          failedAnnouncement="Message could not be copied."
          className="message-copy"
        />
      )}
      <span className="message-footer-spacer" />
      {timestamp && (
        <WithTooltip label={timestamp.full}>
          <time dateTime={createdAt}>
            {timestamp.short}
          </time>
        </WithTooltip>
      )}
    </footer>
  );
}

/** Write the rendered message for rich editors and its source text everywhere. */
export async function copyMessageContent(
  html: string,
  text: string,
  clipboard: ClipboardWriter | undefined = globalThis.navigator?.clipboard,
  item: typeof ClipboardItem | undefined = globalThis.ClipboardItem,
): Promise<void> {
  if (html.trim()) {
    await copyRichText(html, text, clipboard, item);
    return;
  }
  await copyPlainText(text, clipboard);
}

export function formatMessageTimestamp(
  createdAt: string,
  now: Date,
): { short: string; full: string } | null {
  const created = new Date(createdAt);
  if (Number.isNaN(created.getTime())) return null;

  const time = new Intl.DateTimeFormat(undefined, {
    hour: "numeric",
    minute: "2-digit",
  }).format(created);
  const startOfToday = new Date(
    now.getFullYear(),
    now.getMonth(),
    now.getDate(),
  );
  const startOfCreatedDay = new Date(
    created.getFullYear(),
    created.getMonth(),
    created.getDate(),
  );
  const dayDifference = Math.round(
    (startOfToday.getTime() - startOfCreatedDay.getTime()) / 86_400_000,
  );

  let short: string;
  if (dayDifference === 0) {
    short = time;
  } else if (dayDifference === 1) {
    short = `Yesterday, ${time}`;
  } else {
    short = new Intl.DateTimeFormat(undefined, {
      month: "short",
      day: "numeric",
      year: created.getFullYear() === now.getFullYear() ? undefined : "numeric",
      hour: "numeric",
      minute: "2-digit",
    }).format(created);
  }

  return {
    short,
    full: new Intl.DateTimeFormat(undefined, {
      weekday: "long",
      month: "long",
      day: "numeric",
      year: "numeric",
      hour: "numeric",
      minute: "2-digit",
    }).format(created),
  };
}
