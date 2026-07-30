import { Logomark } from "./Logomark";

/**
 * One renderer-owned status for an open foreground turn.
 *
 * Reasoning used to raise a visible "Thinking…" label here because the text
 * behind it was withheld. It is not withheld any more: the thinking accordion
 * on the assistant bubble says the same thing and shows the reasoning, so this
 * is back to being the plain working state.
 */
export function AssistantWorkingIndicator() {
  return (
    <div
      className="assistant-working"
      role="status"
      aria-live="polite"
      aria-atomic="true"
    >
      <Logomark className="assistant-working-mark" />
      {/* The working state is carried by the animated logomark alone; its
          label stays available to assistive tech without crowding the
          transcript. */}
      <span className="sr-only">Working</span>
    </div>
  );
}
