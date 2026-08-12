/**
 * Returning a deep link to the exact place a conversation paused.
 *
 * The transcript has no addressable positions of its own — it is a stream that
 * follows its own end. What it does have is the parked call each waiting card
 * decides, which the inbox already names, so a card is found by that id and the
 * scroll is put on it rather than at the bottom.
 */

/** How the DOM node of a waiting card is labelled. See `MessageList`. */
export const PENDING_CALL_ATTRIBUTE = "data-pending-call-id";

/** The class a revealed card wears while it is being pointed out. */
export const FOCUS_CLASS = "is-deep-linked";

/** How long the reveal highlight stays up. */
export const FOCUS_HIGHLIGHT_MS = 2_000;

/**
 * Put the scroll on the card deciding `callId`, if it is on screen yet.
 *
 * `false` means the card is not mounted — the conversation is still loading, or
 * the item was answered elsewhere — and the caller decides whether to wait for
 * it. Never scrolls to a guess: an absent card leaves the transcript where the
 * reader left it.
 */
export function revealPendingCall(
  container: Element | null,
  callId: string,
): boolean {
  const card = findPendingCard(container, callId);
  if (!card) return false;
  card.scrollIntoView({ block: "center", behavior: "smooth" });
  card.classList.add(FOCUS_CLASS);
  setTimeout(() => card.classList.remove(FOCUS_CLASS), FOCUS_HIGHLIGHT_MS);
  return true;
}

/**
 * The card deciding `callId`, or `null`.
 *
 * The id is written into a selector, so it is checked against the shape ids
 * actually take before it gets there. Anything else is treated as no match
 * rather than escaped, because a value that is not an id cannot name a card.
 */
export function findPendingCard(
  container: Element | null,
  callId: string,
): HTMLElement | null {
  if (!container || !/^[A-Za-z0-9_-]{1,128}$/.test(callId)) return null;
  return container.querySelector<HTMLElement>(
    `[${PENDING_CALL_ATTRIBUTE}="${callId}"]`,
  );
}
