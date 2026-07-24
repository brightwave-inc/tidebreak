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
      <span>{thinking ? "Thinking…" : "Working"}</span>
    </div>
  );
}
