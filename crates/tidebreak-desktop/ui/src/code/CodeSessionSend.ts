import type { CodeTurnSnapshot } from "../api/types";
import { applyAcceptedTurn, type CodeSessionState } from "./CodeSessionReducer";

/**
 * Insert a user turn only after the server accepts it.
 *
 * An optimistic bubble that survives a failed submit cannot be answered and
 * stacks a duplicate on retry. Chat removes its optimistic item in the catch;
 * here the cheaper mirror is to wait for the accepted snapshot, which is also
 * the hydrate key.
 */
export async function submitAcceptedTurn(
  update: (change: (session: CodeSessionState) => CodeSessionState) => void,
  submit: () => Promise<CodeTurnSnapshot>,
): Promise<CodeTurnSnapshot> {
  const turn = await submit();
  update((session) => applyAcceptedTurn(session, turn));
  return turn;
}
