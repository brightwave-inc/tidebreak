import type { AgentRun, SandboxAgentCancellation } from "./api";

export type SandboxAgentStopToken = Readonly<{
  chatId: string;
  runId: string;
  generation: number;
  request: number;
}>;

/**
 * Keeps async stop completions tied to one exact chat selection and sandbox
 * run. Invalidating the fence makes every in-flight completion stale.
 */
export class SandboxAgentStopFence {
  private generation = 0;
  private request = 0;
  private readonly active = new Map<string, number>();

  begin(chatId: string, runId: string): SandboxAgentStopToken | null {
    const key = sandboxAgentStopKey(chatId, runId);
    if (this.active.has(key)) return null;
    const request = ++this.request;
    this.active.set(key, request);
    return { chatId, runId, generation: this.generation, request };
  }

  isCurrent(token: SandboxAgentStopToken, selectedChatId: string | null): boolean {
    return (
      token.generation === this.generation &&
      token.chatId === selectedChatId &&
      this.active.get(sandboxAgentStopKey(token.chatId, token.runId)) === token.request
    );
  }

  finish(token: SandboxAgentStopToken, selectedChatId: string | null): boolean {
    if (!this.isCurrent(token, selectedChatId)) return false;
    this.active.delete(sandboxAgentStopKey(token.chatId, token.runId));
    return true;
  }

  invalidate(): void {
    this.generation += 1;
    this.active.clear();
  }
}

export function sandboxAgentStopKey(chatId: string, runId: string): string {
  return JSON.stringify([chatId, runId]);
}

export function canStopSandboxAgentRun(run: AgentRun): boolean {
  return (
    run.execution === "sandbox" &&
    ["queued", "running", "waiting", "retry_wait"].includes(run.status)
  );
}

export function reconcileSandboxAgentCancellation(
  runs: AgentRun[],
  cancellation: SandboxAgentCancellation,
): AgentRun[] {
  return runs.map((run) => {
    if (run.id !== cancellation.id || run.execution !== "sandbox") return run;
    if (["completed", "failed", "cancelled"].includes(run.status)) return run;
    return { ...run, status: cancellation.status, activity: null };
  });
}
