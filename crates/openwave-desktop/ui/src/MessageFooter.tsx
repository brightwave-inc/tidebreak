import { useEffect, useState } from "react";

type MessageFooterProps = {
  role: "user" | "assistant";
  text: string;
  createdAt?: string;
  settled?: boolean;
};

type ClipboardWriter = {
  writeText(text: string): Promise<void>;
};

type CopyState = "idle" | "copied" | "failed";

const COPY_STATE_RESET_MS = 3_000;

export function MessageFooter({
  role,
  text,
  createdAt,
  settled = true,
}: MessageFooterProps) {
  const [copyState, setCopyState] = useState<CopyState>("idle");
  const hasContent = text.trim().length > 0;
  const timestamp = createdAt && (role === "user" || (settled && hasContent))
    ? formatMessageTimestamp(createdAt, new Date())
    : null;
  const canCopy = role === "assistant" && settled && hasContent;

  useEffect(() => {
    if (copyState === "idle") return;
    const timeout = window.setTimeout(
      () => setCopyState("idle"),
      COPY_STATE_RESET_MS,
    );
    return () => window.clearTimeout(timeout);
  }, [copyState]);

  if (!canCopy && !timestamp) return null;

  async function onCopy() {
    try {
      await copyMessageText(text);
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
  }

  const copyLabel =
    copyState === "copied"
      ? "Copied"
      : copyState === "failed"
        ? "Copy failed"
        : "Copy";

  return (
    <footer className="message-footer">
      {canCopy && (
        <button
          type="button"
          className="message-copy"
          aria-label={copyLabel}
          title={copyLabel}
          onClick={() => void onCopy()}
        >
          <span aria-hidden="true">{copyState === "copied" ? "✓" : "⧉"}</span>
          <span>{copyLabel}</span>
        </button>
      )}
      <span className="message-footer-spacer" />
      {timestamp && (
        <time dateTime={createdAt} title={timestamp.full}>
          {timestamp.short}
        </time>
      )}
      <span className="sr-only" role="status" aria-live="polite">
        {copyState === "copied"
          ? "Message copied to clipboard."
          : copyState === "failed"
            ? "Message could not be copied."
            : ""}
      </span>
    </footer>
  );
}

export async function copyMessageText(
  text: string,
  clipboard: ClipboardWriter | undefined = globalThis.navigator?.clipboard,
): Promise<void> {
  if (!clipboard?.writeText) {
    throw new Error("Clipboard access is unavailable");
  }
  await clipboard.writeText(text);
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
