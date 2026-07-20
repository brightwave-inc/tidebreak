import { Logomark } from "./Logomark";

/** One fixed, renderer-owned status for an open foreground turn. */
export function AssistantWorkingIndicator() {
  return (
    <div
      className="assistant-working"
      role="status"
      aria-live="polite"
      aria-atomic="true"
    >
      <Logomark className="assistant-working-mark" />
      <span>Working</span>
    </div>
  );
}
