import type { AgentEvent } from "./api";

/**
 * Agent-run mutations performed by tools are query-visible at the durable
 * completion boundary, not when the provider first announces the call.
 */
export function shouldRefreshAgentRunsAfterToolEvent(
  event: AgentEvent,
): boolean {
  return event.type === "tool_call_completed";
}
