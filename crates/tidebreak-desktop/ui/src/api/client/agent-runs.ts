import type {
  AgentActivityHistoryEntry,
  AgentRun,
  AgentRunProgress,
  AgentRunTaskPlan,
  SandboxAgentCancellation,
  TaskPlan,
} from "../types";
import {
  parseAgentActivityHistory,
  parseAgentRunProgress,
  parseAgentRunTaskPlan,
  parseSandboxAgentCancellation,
  parseTaskPlan,
} from "../parsers";
import { type Constructor, HttpCore } from "./http";

/** Agent runs, task plans, and run progress. */
export function withAgentRunsApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    /**
     * The conversation's current task plan, or `null` when it has none.
     *
     * The journal only carries a hint that the plan moved on, so this is where
     * the steps come from — on the hint, and again on reload. The payload is
     * model-authored text, so it is validated here rather than trusted.
     */
    async getTaskPlan(chatId: string): Promise<TaskPlan | null> {
      const body = await this.json<unknown>(
        `/chats/${encodeURIComponent(chatId)}/task-plan`,
        { headers: this.headers() },
      );
      return parseTaskPlan(body);
    }

    listAgentRuns(chatId: string): Promise<AgentRun[]> {
      return this.json(`/chats/${chatId}/agent-runs`, {
        headers: this.headers(),
      });
    }

    /**
     * The ordered, renderer-safe activity history for one background run.
     *
     * Malformed or unknown entries are dropped rather than trusted, keeping the
     * closed vocabulary the server promises. A wrong-chat, foreground, or missing
     * run answers `404`, which surfaces as a thrown error.
     */
    async listAgentRunActivity(
      chatId: string,
      runId: string,
    ): Promise<AgentActivityHistoryEntry[]> {
      const body = await this.json<unknown>(
        `/chats/${encodeURIComponent(chatId)}/agent-runs/${encodeURIComponent(runId)}/activity`,
        { headers: this.headers() },
      );
      return parseAgentActivityHistory(body);
    }

    /**
     * The full ordered checklist one background run keeps, or `null`.
     *
     * The run snapshot already carries the count and the current step, which is
     * all a status row needs; this is the list behind it, read when a reader
     * opens the run. A wrong-chat, foreground, or missing run answers `404`,
     * which surfaces as a thrown error.
     */
    async getAgentRunTaskPlan(
      chatId: string,
      runId: string,
    ): Promise<AgentRunTaskPlan | null> {
      const body = await this.json<unknown>(
        `/chats/${encodeURIComponent(chatId)}/agent-runs/${encodeURIComponent(runId)}/task-plan`,
        { headers: this.headers() },
      );
      return parseAgentRunTaskPlan(body);
    }

    /**
     * One resumable page of a background run's live progress.
     *
     * Poll with the previous page's `nextSequence` to receive only what the run
     * has published since. A wrong-chat, foreground, or missing run answers
     * `404`, which surfaces as a thrown error.
     */
    async listAgentRunProgress(
      chatId: string,
      runId: string,
      afterSequence = 0,
      limit?: number,
    ): Promise<AgentRunProgress> {
      const query = new URLSearchParams({
        after_sequence: String(afterSequence),
      });
      if (limit !== undefined) query.set("limit", String(limit));
      const body = await this.json<unknown>(
        `/chats/${encodeURIComponent(chatId)}/agent-runs/${encodeURIComponent(runId)}/progress?${query}`,
        { headers: this.headers() },
      );
      return parseAgentRunProgress(body, afterSequence);
    }

    /** Resume a background run paused at a check-in, optionally with guidance. */
    async resumeAgentRun(
      chatId: string,
      runId: string,
      guidance?: string,
    ): Promise<void> {
      await this.json<unknown>(
        `/chats/${encodeURIComponent(chatId)}/agent-runs/${encodeURIComponent(runId)}/resume`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify(guidance ? { guidance } : {}),
        },
        202,
      );
    }

    async cancelAgentRun(
      chatId: string,
      runId: string,
    ): Promise<SandboxAgentCancellation> {
      const body = await this.json<unknown>(
        `/chats/${encodeURIComponent(chatId)}/agent-runs/${encodeURIComponent(runId)}/cancel`,
        {
          method: "POST",
          headers: this.headers(),
        },
        202,
      );
      const cancellation = parseSandboxAgentCancellation(body);
      if (!cancellation || cancellation.id !== runId) {
        throw new Error("sandbox cancellation response is invalid");
      }
      return cancellation;
    }
  };
}
