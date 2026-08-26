import type { PullRequestComment } from "../api/types";
import type { StatusTone } from "./statusTone";

/**
 * Conversation and diff presentation for a pull request: comment ordering,
 * file-status vocabulary, avatars, and emoji. The state itself — lifecycle,
 * gate, tones, labels — lives in `prState.ts`; nothing here decides a color.
 */

export type PullRequestCommentOrder = "newest" | "oldest";

/**
 * The conversation in the reader's chosen order. Hosts hand comments over
 * oldest-first; the panel defaults to newest-first because the reason a
 * reader opens a busy pull request is almost always the latest verdict, not
 * the greeting. The sort is stable, so comments without a parseable
 * timestamp keep the host's relative order and sink to the bottom of the
 * newest-first view rather than claiming the top.
 */
export function orderPullRequestComments(
  comments: readonly PullRequestComment[],
  order: PullRequestCommentOrder,
): PullRequestComment[] {
  const time = (comment: PullRequestComment): number => {
    if (!comment.created_at) return 0;
    const parsed = Date.parse(comment.created_at);
    return Number.isNaN(parsed) ? 0 : parsed;
  };
  return [...comments].sort((left, right) =>
    order === "newest" ? time(right) - time(left) : time(left) - time(right),
  );
}

/** GitHub's diff-status vocabulary, as a short verb the row can show. */
export const FILE_STATUS_LABEL: Readonly<Record<string, string>> = {
  added: "Added",
  removed: "Removed",
  modified: "Modified",
  renamed: "Renamed",
  copied: "Copied",
  changed: "Changed",
  unchanged: "Unchanged",
};

export function fileStatusLabel(status: string): string {
  return FILE_STATUS_LABEL[status] ?? "Changed";
}

export function fileStatusTone(status: string): StatusTone {
  if (status === "added") return "ready";
  if (status === "removed") return "critical";
  return "neutral";
}

/**
 * A GitHub avatar for a login, when the host did not send one.
 *
 * Only well-formed logins are turned into URLs, so a display name never
 * becomes a request.
 */
export function githubAvatarUrl(
  author: string | undefined,
): string | undefined {
  if (!author || !/^[A-Za-z0-9-]+$/.test(author)) return undefined;
  return `https://github.com/${encodeURIComponent(author)}.png?size=64`;
}

const GITHUB_EMOJI: Readonly<Record<string, string>> = {
  "+1": "👍",
  "-1": "👎",
  bug: "🐛",
  checkered_flag: "🏁",
  eyes: "👀",
  fire: "🔥",
  heart: "❤️",
  heavy_check_mark: "✔️",
  laughing: "😆",
  memo: "📝",
  party_parrot: "🦜",
  rocket: "🚀",
  shipit: "🐿️",
  smile: "😄",
  sparkles: "✨",
  tada: "🎉",
  thinking: "🤔",
  warning: "⚠️",
  wave: "👋",
  white_check_mark: "✅",
  x: "❌",
};

/** Expand common GitHub emoji shortcodes outside inline and fenced code. */
export function expandGithubEmojiShortcodes(markdown: string): string {
  return markdown
    .split(/(```[\s\S]*?(?:```|$)|`[^`\n]*`)/g)
    .map((part) => {
      if (part.startsWith("`")) return part;
      return part.replace(/:([+\-a-z0-9_]+):/gi, (token, name: string) => {
        return GITHUB_EMOJI[name.toLowerCase()] ?? token;
      });
    })
    .join("");
}
