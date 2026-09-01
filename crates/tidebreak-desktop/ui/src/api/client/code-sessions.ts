import type {
  CodeForkTranscript,
  CodeSessionSnapshot,
  CodeTurnSnapshot,
  HarnessKind,
  PermissionMode,
  QueuedCodeTurn,
  ReasoningEffort,
} from "../types";
import {
  type CodeTurnSubmission,
  parseCodeForkTranscript,
  parseCodeSession,
  parseCodeSessionList,
  parseCodeTurnList,
  parseCodeTurnSubmission,
  parseQueuedCodeTurn,
} from "../../code/parsers";
import { type Constructor, HttpCore, requireParsed } from "./http";

/** Code sessions: turns, settings, queue, steer, fork, and reap. */
export function withCodeSessionsApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    async listCodeWorkspaceSessions(
      workspaceId: string,
    ): Promise<CodeSessionSnapshot[]> {
      const body = await this.json<unknown>(
        `/code/workspaces/${encodeURIComponent(workspaceId)}/sessions`,
        { headers: this.headers() },
      );
      return requireParsed(
        parseCodeSessionList(body),
        "code workspace sessions",
      );
    }

    async createCodeSession(
      workspaceId: string,
      body: {
        harness: HarnessKind;
        permission_mode: PermissionMode;
        model?: string;
        reasoning_effort?: ReasoningEffort;
        fast_mode?: boolean;
      },
    ): Promise<CodeSessionSnapshot> {
      return requireParsed(
        parseCodeSession(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/sessions`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(body),
            },
          ),
        ),
        "code session",
      );
    }

    async getCodeSessionDebug(sessionId: string): Promise<unknown> {
      return this.json(
        `/code/sessions/${encodeURIComponent(sessionId)}/debug`,
        {
          headers: this.headers(),
        },
      );
    }

    async listCodeSessionTurns(sessionId: string): Promise<CodeTurnSnapshot[]> {
      const body = await this.json<unknown>(
        `/code/sessions/${encodeURIComponent(sessionId)}/turns`,
        { headers: this.headers() },
      );
      return requireParsed(parseCodeTurnList(body), "code session turns");
    }

    /**
     * Submit a turn, or park it behind the one in flight.
     *
     * The route answers 202 either way: a turn snapshot when the session was
     * idle, a queue receipt when it was busy. Both are accepted work — treating
     * the receipt as a malformed turn would report a failure for a message the
     * server holds, and a retry would double-send.
     */
    async submitCodeTurn(
      sessionId: string,
      message: string,
      model?: string,
      attachments?: readonly { blob_id: string; media_type: string }[],
      /**
       * Omit to leave the session's stored level alone. Pass `null` to hand the
       * level back to the engine's own default — that is a choice, not an
       * omission, so it cannot ride on `undefined`.
       */
      reasoningEffort?: ReasoningEffort | null,
    ): Promise<CodeTurnSubmission> {
      return requireParsed(
        parseCodeTurnSubmission(
          await this.json(
            `/code/sessions/${encodeURIComponent(sessionId)}/turns`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify({
                message,
                ...(model ? { model } : {}),
                ...(reasoningEffort !== undefined
                  ? { reasoning_effort: reasoningEffort }
                  : {}),
                ...(attachments && attachments.length > 0
                  ? { attachments }
                  : {}),
              }),
            },
          ),
        ),
        "code turn",
      );
    }

    /**
     * Move a live session onto a different permission mode.
     *
     * The engine re-postures in place where it can and is relaunched where it
     * cannot; either way the new mode governs from the next turn. Refused (409
     * `turn_running`) while a turn is in flight.
     */
    async setCodeSessionPermissionMode(
      sessionId: string,
      permissionMode: PermissionMode,
    ): Promise<CodeSessionSnapshot> {
      return requireParsed(
        parseCodeSession(
          await this.json(
            `/code/sessions/${encodeURIComponent(sessionId)}/mode`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify({ permission_mode: permissionMode }),
            },
          ),
        ),
        "code session",
      );
    }

    /** Change a session's reasoning effort. `null` is the engine's own default. */
    async setCodeSessionReasoningEffort(
      sessionId: string,
      reasoningEffort: ReasoningEffort | null,
    ): Promise<CodeSessionSnapshot> {
      return requireParsed(
        parseCodeSession(
          await this.json(
            `/code/sessions/${encodeURIComponent(sessionId)}/effort`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify({ reasoning_effort: reasoningEffort }),
            },
          ),
        ),
        "code session",
      );
    }

    /** Arm or disarm the engine's fast mode for a session. */
    async setCodeSessionFastMode(
      sessionId: string,
      fastMode: boolean,
    ): Promise<CodeSessionSnapshot> {
      return requireParsed(
        parseCodeSession(
          await this.json(
            `/code/sessions/${encodeURIComponent(sessionId)}/fast-mode`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify({ fast_mode: fastMode }),
            },
          ),
        ),
        "code session",
      );
    }

    getCodeSessionImage(
      sessionId: string,
      blobId: string,
      signal?: AbortSignal,
    ): Promise<Blob> {
      return this.blob(
        `/code/sessions/${encodeURIComponent(sessionId)}/attachments/images/${encodeURIComponent(blobId)}`,
        signal,
      );
    }

    /**
     * Redirect the in-flight turn. The route answers with an empty status;
     * unsupported engines refuse with `steering_unavailable` rather than queue.
     */
    steerCodeSession(
      sessionId: string,
      expectedTurnId: string,
      guidance: string,
    ): Promise<void> {
      return this.json(
        `/code/sessions/${encodeURIComponent(sessionId)}/steer`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify({
            expected_turn_id: expectedTurnId,
            guidance,
          }),
        },
      );
    }

    interruptCodeSession(sessionId: string): Promise<void> {
      return this.json(
        `/code/sessions/${encodeURIComponent(sessionId)}/interrupt`,
        {
          method: "POST",
          headers: this.headers(),
        },
      );
    }

    /** The session's queued messages, FIFO, plus whether promotion is paused. */
    async listCodeQueuedTurns(
      sessionId: string,
    ): Promise<{ queued: QueuedCodeTurn[]; paused: boolean }> {
      const snapshot = await this.json<{ queued: unknown[]; paused: boolean }>(
        `/code/sessions/${encodeURIComponent(sessionId)}/queued`,
        { headers: this.headers() },
      );
      return {
        queued: snapshot.queued.map((row) =>
          requireParsed(parseQueuedCodeTurn(row), "queued code turn"),
        ),
        paused: snapshot.paused === true,
      };
    }

    async patchCodeQueuedTurn(
      sessionId: string,
      queuedId: string,
      update: { message?: string; position?: number },
    ): Promise<QueuedCodeTurn> {
      return requireParsed(
        parseQueuedCodeTurn(
          await this.json(
            `/code/sessions/${encodeURIComponent(sessionId)}/queued/${encodeURIComponent(queuedId)}`,
            {
              method: "PATCH",
              headers: this.headers(true),
              body: JSON.stringify(update),
            },
          ),
        ),
        "queued code turn",
      );
    }

    async deleteCodeQueuedTurn(
      sessionId: string,
      queuedId: string,
    ): Promise<void> {
      await this.json<unknown>(
        `/code/sessions/${encodeURIComponent(sessionId)}/queued/${encodeURIComponent(queuedId)}`,
        { method: "DELETE", headers: this.headers() },
        204,
      );
    }

    async putCodeQueuePaused(
      sessionId: string,
      paused: boolean,
    ): Promise<void> {
      await this.json<unknown>(
        `/code/sessions/${encodeURIComponent(sessionId)}/queue-paused`,
        {
          method: "PUT",
          headers: this.headers(true),
          body: JSON.stringify({ paused }),
        },
        204,
      );
    }

    /** Release a paused queue so the worker starts the head row. */
    async sendCodeQueuedNow(sessionId: string): Promise<void> {
      await this.json<unknown>(
        `/code/sessions/${encodeURIComponent(sessionId)}/queued/send-now`,
        { method: "POST", headers: this.headers() },
        204,
      );
    }

    /**
     * Write one fork of a session — the condensed transcript plus per-turn
     * records — into private storage, for a child agent to read.
     *
     * `atTurnId` forks at the end of that turn; omitted, the fork covers the
     * whole conversation. The child session is created separately: this call
     * only produces the files, so the reader still picks the engine and edits
     * the framing before anything is sent.
     */
    async forkCodeSession(
      sessionId: string,
      atTurnId?: string,
    ): Promise<CodeForkTranscript> {
      return requireParsed(
        parseCodeForkTranscript(
          await this.json(
            `/code/sessions/${encodeURIComponent(sessionId)}/fork`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(atTurnId ? { at_turn: atTurnId } : {}),
            },
          ),
        ),
        "code fork transcript",
      );
    }

    async reapCodeSession(sessionId: string): Promise<CodeSessionSnapshot> {
      return requireParsed(
        parseCodeSession(
          await this.json(
            `/code/sessions/${encodeURIComponent(sessionId)}/reap`,
            {
              method: "POST",
              headers: this.headers(),
            },
          ),
        ),
        "code session",
      );
    }
  };
}
