import type { ReactNode } from "react";
import { ErrorBoundary } from "./ErrorBoundary";
import { ToolActivityUnavailable } from "./ToolActivityGroup";

/**
 * One card, contained.
 *
 * Cards render model-influenced data through several defensive parsers, and one
 * of them being wrong must not cost the card next to it. The siblings that make
 * this matter are the cards the turn is waiting on — an approval prompt, a
 * folder-access request, a question put to the user. A pending decision that
 * renders as nothing leaves the reader with no way to answer, no explanation,
 * and a turn that cannot proceed.
 *
 * `signature` is the data the card draws on, reduced to what decides whether it
 * can render, so a card that threw mid-stream is retried when its call moves on
 * rather than staying broken for the life of the transcript.
 */
export function isolatedCard(
  key: string,
  signature: string,
  card: ReactNode,
  /**
   * The parked call this card decides, when it decides one. It is written to
   * the DOM so a deep link from the inbox can find the card it named — the
   * transcript is otherwise addressable only by scroll position.
   */
  pendingCallId?: string,
): ReactNode {
  return (
    <ErrorBoundary
      key={key}
      resetKey={signature}
      fallback={<ToolActivityUnavailable />}
    >
      {pendingCallId ? (
        <div data-pending-call-id={pendingCallId}>{card}</div>
      ) : (
        card
      )}
    </ErrorBoundary>
  );
}
