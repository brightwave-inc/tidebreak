import { waitingAnnouncement, type NeedsYouQuestion } from "./needsYou";

/**
 * The status region for a question or a plan that stands in for the
 * composer.
 *
 * The card replaces the composer, and the composer's own status region goes
 * with it. This region stays mounted beside both, so the text it announces
 * is a change rather than a region that arrived already full, which screen
 * readers skip. Answering the card clears it.
 */
export function ChatPromptAnnouncer({
  question,
}: {
  question: NeedsYouQuestion | null;
}) {
  return (
    <span className="sr-only" role="status" data-testid="chat-prompt-announcer">
      {question ? waitingAnnouncement(question) : ""}
    </span>
  );
}
