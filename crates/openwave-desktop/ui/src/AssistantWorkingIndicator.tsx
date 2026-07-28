import { Logomark } from "./Logomark";

/** One renderer-owned status for an open foreground turn. */
export function AssistantWorkingIndicator({
  thinking = false,
}: {
  /** The model is emitting reasoning rather than visible output. */
  thinking?: boolean;
}) {
  return (
    <div
      className="assistant-working"
      role="status"
      aria-live="polite"
      aria-atomic="true"
    >
      <Logomark className="assistant-working-mark" />
      {/* The default working state is carried by the animated logomark alone;
          its label stays available to assistive tech without crowding the
          transcript. The thinking state keeps a visible label. */}
      {thinking ? (
        <span>Thinking…</span>
      ) : (
        <span className="sr-only">Working</span>
      )}
    </div>
  );
}
