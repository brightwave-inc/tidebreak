import { type ReactNode } from "react";
import { CircleCheck } from "lucide-react";

import type { PullRequestComment } from "../api/types";
import { MessageMarkdown } from "@/MessageMarkdown";
import { cn } from "@/lib/utils";
import { GithubAvatar } from "./GithubAvatar";
import { expandGithubEmojiShortcodes } from "./pullRequestPresentation";

/**
 * One pull-request comment, drawn the way GitHub draws it: avatar, author,
 * review verdict, file anchor, then the body as real Markdown.
 *
 * Presentation only. The workspace inspector and the delivery panel disagree
 * about what a reader can *do* with a comment — the inspector can attach it to
 * a chat and hide it locally, delivery cannot — so each passes its own menu in
 * through `actions` rather than growing a second copy of this card.
 */
export function PrCommentCard({
  comment,
  resolved = false,
  actions,
  className,
}: {
  comment: PullRequestComment;
  /** Marked resolved locally in Tidebreak, not on GitHub. */
  resolved?: boolean;
  actions?: ReactNode;
  className?: string;
}) {
  const when = comment.created_at
    ? new Date(comment.created_at).toLocaleString(undefined, {
        month: "short",
        day: "numeric",
        hour: "numeric",
        minute: "2-digit",
      })
    : null;
  const author = comment.author ?? "Unknown";
  const anchor =
    comment.kind === "inline" && comment.path
      ? `${comment.path}${comment.line !== undefined ? `:${comment.line}` : ""}`
      : null;

  return (
    <article
      className={cn(
        "border-border-subtle group/comment rounded-xl border bg-background/45 px-2.5 py-2.5",
        resolved && "bg-muted/25 opacity-70",
        className,
      )}
    >
      <div className="flex min-w-0 items-start gap-2">
        <GithubAvatar
          login={comment.author}
          url={comment.avatar_url}
          className="size-7"
        />
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-1.5">
            <span className="text-foreground min-w-0 truncate text-xs font-medium">
              {author}
            </span>
            {comment.kind === "review" && comment.review_state && (
              <span className="text-muted-foreground shrink-0 text-xs capitalize">
                {comment.review_state.replaceAll("_", " ")}
              </span>
            )}
            {resolved && (
              <span className="text-success-foreground flex shrink-0 items-center gap-1 text-2xs font-medium">
                <CircleCheck className="size-3" />
                Resolved here
              </span>
            )}
            {when && (
              <span className="text-muted-foreground ml-auto shrink-0 text-2xs tabular-nums">
                {when}
              </span>
            )}
          </div>
          {anchor && (
            <div
              className="text-muted-foreground mt-0.5 truncate font-mono text-2xs"
              title={anchor}
            >
              {anchor}
            </div>
          )}
        </div>
        {actions}
      </div>
      <div className="review-comment-markdown mt-2 pl-9 text-md leading-5">
        <MessageMarkdown>
          {expandGithubEmojiShortcodes(comment.body)}
        </MessageMarkdown>
      </div>
    </article>
  );
}
