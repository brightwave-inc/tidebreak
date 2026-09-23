import type { ToolActionPreview } from "./api";
import { approvalAsk } from "./ApprovalCard";
import { toolPreviewPresentation } from "./ToolPreview";

/**
 * What an agent that stopped for you is waiting on.
 *
 * Folder access and file write-backs are approvals too: each asks you to let
 * one action through.
 */
export type NeedsYouKind = "approval" | "question" | "plan";

/** The one question an agent is blocked on, in its own words. */
export type NeedsYouQuestion = { kind: NeedsYouKind; text: string };

/** Longest question a banner or an announcement repeats. */
export const MAX_QUESTION_CHARS = 120;

/** Longest conversation name a notification title repeats. */
const MAX_NAME_CHARS = 80;

const NEED: Record<NeedsYouKind, string> = {
  approval: "approval",
  question: "answer",
  plan: "review",
};

/** A notification title: "Needs your approval: Fix the login check". */
export function needsYouTitle(kind: NeedsYouKind, name: string): string {
  return `Needs your ${NEED[kind]}: ${oneLine(name, MAX_NAME_CHARS)}`;
}

/**
 * What a screen reader hears while a turn waits on you:
 * "Waiting for your answer: Which database should I use?"
 */
export function waitingAnnouncement(question: NeedsYouQuestion): string {
  const text = question.text.trim();
  return text
    ? `Waiting for your ${NEED[question.kind]}: ${text}`
    : `Waiting for your ${NEED[question.kind]}`;
}

/**
 * Agent-written text as plain text for a banner: control and format
 * characters removed, so a direction override cannot reorder what the
 * banner shows, and whitespace collapsed.
 */
export function plainText(text: string): string {
  return text
    .replace(/[\p{Cc}\p{Cf}]/gu, (character) =>
      /\s/.test(character) ? " " : "",
    )
    .replace(/\s+/g, " ")
    .trim();
}

/**
 * The first line of `text` with words in it, as plain text, cut to `limit`
 * characters. A banner and a live region both read one line.
 */
export function oneLine(text: string, limit = MAX_QUESTION_CHARS): string {
  const line =
    text
      .split(/\r?\n/)
      .map(plainText)
      .find((part) => part.length > 0) ?? "";
  const chars = Array.from(line);
  if (chars.length <= limit) return line;
  return `${chars
    .slice(0, limit - 1)
    .join("")
    .trimEnd()}…`;
}

/**
 * A Work tool approval as one question. A short ask names the action it is
 * about, "Run this command? npm test"; a long ask is already the whole
 * question and stands alone.
 */
export function workApprovalQuestion(
  summary: string,
  preview: ToolActionPreview | null,
): NeedsYouQuestion {
  const ask = approvalAsk(preview, summary);
  const action =
    ask.summaryLine !== null && preview
      ? oneLine(toolPreviewPresentation(preview).headline)
      : "";
  return {
    kind: "approval",
    text: oneLine(action ? `${ask.title} ${action}` : ask.title),
  };
}
