import { applyAcceptedTurn, type CodeSessionState } from "./CodeSessionReducer";
import type { CodeTurnSubmission } from "./parsers";

/**
 * Insert a user turn only after the server accepts it.
 *
 * An optimistic bubble that survives a failed submit cannot be answered and
 * stacks a duplicate on retry. Chat removes its optimistic item in the catch;
 * here the cheaper mirror is to wait for the accepted snapshot, which is also
 * the hydrate key.
 *
 * A queued follow-up has no turn row yet, so there is nothing to key an item
 * on. Its bubble arrives when the worker promotes the queued row and the
 * session's `turn_started` event pulls the snapshot in.
 */
export async function submitAcceptedTurn(
  update: (change: (session: CodeSessionState) => CodeSessionState) => void,
  submit: () => Promise<CodeTurnSubmission>,
): Promise<CodeTurnSubmission> {
  const outcome = await submit();
  if (outcome.kind === "ran") {
    update((session) => applyAcceptedTurn(session, outcome.turn));
  }
  return outcome;
}
