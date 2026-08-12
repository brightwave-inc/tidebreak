import { Logomark } from "./Logomark";

/**
 * One renderer-owned status for an open foreground turn.
 *
 * Reasoning used to raise a visible "Thinking…" label here because the text
 * behind it was withheld. It is not withheld any more: the thinking accordion
 * on the assistant bubble says the same thing and shows the reasoning, so this
 * is back to being the plain working state — unless semantic compaction is
 * running, which needs a visible label distinct from ordinary working.
 */
export function AssistantWorkingIndicator({
  compacting = false,
}: {
  compacting?: boolean;
}) {
  return (
    <div
      className={`assistant-working${compacting ? " is-compacting" : ""}`}
      role="status"
      aria-live="polite"
      aria-atomic="true"
    >
      <Logomark className="assistant-working-mark" />
      {compacting ? (
        <span className="assistant-working-label">Compacting conversation</span>
      ) : (
        // The working state is carried by the animated logomark alone; its
        // label stays available to assistive tech without crowding the
        // transcript.
        <span className="sr-only">Working</span>
      )}
    </div>
  );
}
