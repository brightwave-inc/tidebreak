import type { ReactNode } from "react";

import { MessageMarkdown } from "./MessageMarkdown";
import { MessageFooter } from "./MessageFooter";
import { splitPastedText } from "./PastedText";
import { PastedTextBlock } from "./PastedTextBlock";

type UserMessageProps = {
  /** What the reader sent, rendered as Markdown like the rest of the turn. */
  text: string;
  /** ISO timestamp behind the footer's relative time, when the mode has one. */
  createdAt?: string;
  /** Landing point for transcript-rail and deep-link jumps. */
  anchorId?: string;
  /**
   * Who sent it, when that is someone other than the reader. A shared session
   * has several contributors (decision 0086); a session with one reads as
   * "You" the way it always did.
   */
  author?: string;
  /** Rendered above the prose — chat puts its image and file attachments here. */
  leading?: ReactNode;
  /** Rendered below the prose — chat puts the skills the turn invoked here. */
  trailing?: ReactNode;
};

/**
 * One thing the reader said, in the frame every mode's transcript uses: the
 * bubble carries the prose, the footer under it carries the timestamp.
 *
 * The slots exist because the frame is shared but its contents are not — chat
 * hangs attachments and invoked skills off a user turn, and code mode has
 * neither. Keeping the structure here rather than in each mode's transcript is
 * what makes a user message read the same in both.
 */
export function UserMessage({
  text,
  createdAt,
  anchorId,
  author,
  leading,
  trailing,
}: UserMessageProps) {
  // A long paste went out folded behind a chip; it comes back folded too.
  const { prose, pasted } = splitPastedText(text);
  return (
    <div className="message-user-frame">
      <article
        className="message message-user"
        aria-label={author ?? "You"}
        data-transcript-anchor={anchorId}
      >
        {author && (
          <p className="text-muted-foreground mb-1 text-xs">{author}</p>
        )}
        {leading}
        {prose && <MessageMarkdown>{prose}</MessageMarkdown>}
        {pasted.map((block, index) => (
          <PastedTextBlock key={`${index}:${block.length}`} text={block} />
        ))}
        {trailing}
      </article>
      <MessageFooter role="user" text={text} createdAt={createdAt} />
    </div>
  );
}
