import type {
  ApprovalGrantRung,
  Chat,
  ChatFrame,
  ChatTurnStarted,
  EditTurnBody,
  ConsentStatementSnapshot,
  PendingFolderAccessRequest,
  PendingOutputWritebackRequest,
  PendingPlanApproval,
  PendingToolApproval,
  PendingUserQuestions,
  PlanDecision,
  QueuedTurn,
  StandingGrantSnapshot,
  UserQuestionAnswer,
} from "../types";
import {
  type Constructor,
  HttpCore,
  WS_HANDSHAKE,
  WS_TOKEN_PREFIX,
} from "./http";
import {
  parseFolderAccessRequest,
  parseOutputWritebackRequest,
  parsePendingPlanApproval,
  parsePendingToolApproval,
  parsePendingUserQuestions,
} from "../parsers";

/** Sending, queueing, steering, approvals, grants, and the chat event socket. */
export function withTurnsApi<TBase extends Constructor<HttpCore>>(Base: TBase) {
  return class extends Base {
    /**
     * `attachments` names images already published for this chat, in the order
     * they should be shown to the model. Only identity crosses: the server
     * re-derives every attachment's format and dimensions from the stored bytes.
     *
     * `invokedSkills` names the skills the reader explicitly reached for. A name
     * the install cannot run refuses the whole turn rather than being dropped, so
     * the caller must be ready to show the refusal.
     */
    postMessage(
      chatId: string,
      turnId: string,
      content: string,
      attachments: readonly string[] = [],
      fileAttachments: readonly string[] = [],
      invokedSkills: readonly string[] = [],
      voiceInputUsed = false,
      queue = false,
    ): Promise<void> {
      return this.json(`/chats/${chatId}/messages`, {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({
          turn_id: turnId,
          content,
          attachments,
          file_attachments: fileAttachments,
          invoked_skills: invokedSkills,
          voice_input_used: voiceInputUsed,
          queue,
        }),
      });
    }

    /**
     * Continue the latest turn as `newTurnId` after it failed or was stopped.
     * The retried turn stays in the conversation, so the model builds on
     * what it already did instead of doing it again.
     */
    retryTurn(
      chatId: string,
      turnId: string,
      newTurnId: string,
    ): Promise<ChatTurnStarted> {
      return this.json(
        `/chats/${encodeURIComponent(chatId)}/turns/${encodeURIComponent(turnId)}/retry`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify({ new_turn_id: newTurnId }),
        },
      );
    }

    /**
     * Answer the latest message again as `newTurnId`. `model` answers under
     * that model this once; the chat keeps its own for the turns after.
     *
     * The earlier answer stays as a version the transcript can page back to.
     * When it acted outside the conversation, the server answers in a new
     * chat instead and says so.
     */
    regenerateTurn(
      chatId: string,
      turnId: string,
      newTurnId: string,
      model?: string,
    ): Promise<ChatTurnStarted> {
      return this.json(
        `/chats/${encodeURIComponent(chatId)}/turns/${encodeURIComponent(turnId)}/regenerate`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify(
            model
              ? { new_turn_id: newTurnId, model }
              : { new_turn_id: newTurnId },
          ),
        },
      );
    }

    /**
     * Replace the latest message and answer the new one. The answer says
     * whether the edit started a new chat instead, because the turn it
     * replaces, or an earlier answer to the same message, changed things
     * outside the conversation.
     */
    editTurn(
      chatId: string,
      turnId: string,
      body: EditTurnBody,
    ): Promise<ChatTurnStarted> {
      return this.json(
        `/chats/${encodeURIComponent(chatId)}/turns/${encodeURIComponent(turnId)}/edit`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify(body),
        },
      );
    }

    /** Start a new chat with a copy of this one's history through `turnId`. */
    branchTurn(chatId: string, turnId: string): Promise<Chat> {
      return this.json(
        `/chats/${encodeURIComponent(chatId)}/turns/${encodeURIComponent(turnId)}/branch`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify({}),
        },
      );
    }

    /** The chat's queued messages, FIFO, plus whether promotion is paused. */
    listQueuedTurns(
      chatId: string,
    ): Promise<{ queued: QueuedTurn[]; paused: boolean }> {
      return this.json(`/chats/${encodeURIComponent(chatId)}/queued`, {
        headers: this.headers(),
      });
    }

    patchQueuedTurn(
      chatId: string,
      turnId: string,
      update: { content?: string; position?: number },
    ): Promise<QueuedTurn> {
      return this.json(
        `/chats/${encodeURIComponent(chatId)}/queued/${encodeURIComponent(turnId)}`,
        {
          method: "PATCH",
          headers: this.headers(true),
          body: JSON.stringify(update),
        },
      );
    }

    async deleteQueuedTurn(chatId: string, turnId: string): Promise<void> {
      await this.json<unknown>(
        `/chats/${encodeURIComponent(chatId)}/queued/${encodeURIComponent(turnId)}`,
        { method: "DELETE", headers: this.headers() },
        204,
      );
    }

    async putQueuePaused(chatId: string, paused: boolean): Promise<void> {
      await this.json<unknown>(
        `/chats/${encodeURIComponent(chatId)}/queue-paused`,
        {
          method: "PUT",
          headers: this.headers(true),
          body: JSON.stringify({ paused }),
        },
        204,
      );
    }

    /** Release a paused queue so the oldest message starts on the next sweep. */
    async sendQueuedNow(chatId: string): Promise<void> {
      await this.json<unknown>(
        `/chats/${encodeURIComponent(chatId)}/queued/send-now`,
        { method: "POST", headers: this.headers() },
        204,
      );
    }

    steer(
      chatId: string,
      turnId: string,
      steerId: string,
      content: string,
      interrupt = false,
      voiceInputUsed = false,
      invokedSkills: readonly string[] = [],
    ): Promise<void> {
      return this.json(`/chats/${chatId}/steer`, {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({
          steer_id: steerId,
          turn_id: turnId,
          content,
          interrupt,
          voice_input_used: voiceInputUsed,
          invoked_skills: invokedSkills,
        }),
      });
    }

    cancel(chatId: string, turnId: string): Promise<void> {
      return this.json(`/chats/${chatId}/cancel`, {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({ turn_id: turnId }),
      });
    }

    decideApproval(
      chatId: string,
      callId: string,
      decision: "approve" | "reject",
      grant: ApprovalGrantRung | null = null,
    ): Promise<void> {
      return this.json(`/chats/${chatId}/approvals/${callId}`, {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({ decision, grant }),
      });
    }

    async listPendingApprovals(chatId: string): Promise<PendingToolApproval[]> {
      const body = await this.json<unknown>(`/chats/${chatId}/approvals`, {
        headers: this.headers(),
      });
      if (!Array.isArray(body)) {
        throw new Error("pending approval response is not an array");
      }

      const approvals = new Map<string, PendingToolApproval>();
      let turnId: string | null = null;
      for (const item of body) {
        const approval = parsePendingToolApproval(item);
        if (!approval) {
          throw new Error("pending approval response contains an invalid item");
        }
        if (approvals.has(approval.callId)) {
          throw new Error(
            "pending approval response contains a duplicate call",
          );
        }
        if (turnId !== null && turnId !== approval.turnId) {
          throw new Error("pending approval response spans multiple turns");
        }
        turnId = approval.turnId;
        approvals.set(approval.callId, approval);
      }
      return [...approvals.values()];
    }

    /** Every standing "don't ask again", newest first, across all chats. */
    listStandingGrants(): Promise<StandingGrantSnapshot[]> {
      return this.json(`/grants`, { headers: this.headers() });
    }

    /**
     * The server's rows of the unified consent read model: every standing tool
     * grant as one consent statement. The capability half comes from the host
     * broker over the Tauri boundary and joins these rows renderer-side.
     */
    listConsentStatements(): Promise<ConsentStatementSnapshot[]> {
      return this.json(`/consent/statements`, { headers: this.headers() });
    }

    /** Withdraw a standing grant; later matching calls ask again. */
    revokeStandingGrant(sourceCallId: string): Promise<void> {
      return this.json(`/grants/${sourceCallId}`, {
        method: "DELETE",
        headers: this.headers(),
      });
    }

    async listPendingFolderAccessRequests(
      chatId: string,
    ): Promise<PendingFolderAccessRequest[]> {
      const body = await this.json<unknown>(
        `/chats/${chatId}/client-executions/pending`,
        { headers: this.headers() },
      );
      if (!Array.isArray(body)) return [];

      const requests = new Map<string, PendingFolderAccessRequest>();
      for (const item of body) {
        const request = parseFolderAccessRequest(item);
        if (request && !requests.has(request.callId)) {
          requests.set(request.callId, request);
        }
      }
      return [...requests.values()];
    }

    async listPendingOutputWritebackRequests(
      chatId: string,
    ): Promise<PendingOutputWritebackRequest[]> {
      const body = await this.json<unknown>(
        `/chats/${chatId}/output-writebacks/pending`,
        { headers: this.headers() },
      );
      if (!Array.isArray(body)) {
        throw new Error("pending output write-back response is not an array");
      }

      const requests = new Map<string, PendingOutputWritebackRequest>();
      for (const item of body) {
        const request = parseOutputWritebackRequest(item);
        if (!request || requests.has(request.callId)) {
          throw new Error(
            "pending output write-back response contains invalid data",
          );
        }
        requests.set(request.callId, request);
      }
      return [...requests.values()];
    }

    async listPendingUserQuestions(
      chatId: string,
    ): Promise<PendingUserQuestions[]> {
      const body = await this.json<unknown>(
        `/chats/${chatId}/questions/pending`,
        {
          headers: this.headers(),
        },
      );
      if (!Array.isArray(body)) {
        throw new Error("pending question response is not an array");
      }
      const requests = new Map<string, PendingUserQuestions>();
      for (const item of body) {
        const request = parsePendingUserQuestions(item);
        if (!request || requests.has(request.callId)) {
          throw new Error("pending question response contains invalid data");
        }
        requests.set(request.callId, request);
      }
      return [...requests.values()];
    }

    async answerUserQuestions(
      chatId: string,
      callId: string,
      answers: UserQuestionAnswer[],
      additionalUserContext?: string,
    ): Promise<void> {
      await this.json(`/chats/${chatId}/questions/${callId}/answer`, {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({
          answers: answers.map((answer) => ({
            question_id: answer.questionId,
            selected_option_ids: answer.selectedOptionIds,
            ...(answer.customAnswer === undefined
              ? {}
              : { custom_answer: answer.customAnswer }),
          })),
          ...(additionalUserContext === undefined
            ? {}
            : { additional_user_context: additionalUserContext }),
        }),
      });
    }

    async listPendingPlanApprovals(
      chatId: string,
    ): Promise<PendingPlanApproval[]> {
      const body = await this.json<unknown>(`/chats/${chatId}/plans/pending`, {
        headers: this.headers(),
      });
      if (!Array.isArray(body)) {
        throw new Error("pending plan response is not an array");
      }
      const requests = new Map<string, PendingPlanApproval>();
      for (const item of body) {
        const request = parsePendingPlanApproval(item);
        if (!request || requests.has(request.callId)) {
          throw new Error("pending plan response contains invalid data");
        }
        requests.set(request.callId, request);
      }
      return [...requests.values()];
    }

    async decidePlan(
      chatId: string,
      callId: string,
      decision: PlanDecision,
    ): Promise<void> {
      await this.json(`/chats/${chatId}/plans/${callId}/decision`, {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify(
          decision.decision === "reject" && decision.feedback !== undefined
            ? { decision: "reject", feedback: decision.feedback }
            : { decision: decision.decision },
        ),
      });
    }

    /** Open the chat event stream; auth via Sec-WebSocket-Protocol. */
    openEvents(
      chatId: string,
      after: number,
      onFrame: (frame: ChatFrame) => void,
    ): WebSocket {
      const url = `${this.baseUrl.replace(/^http/, "ws")}/chats/${chatId}/events?after=${after}`;
      const protocols = [WS_HANDSHAKE, `${WS_TOKEN_PREFIX}${this.token}`];
      const socket = new WebSocket(url, protocols);
      socket.onmessage = (msg) => {
        try {
          onFrame(JSON.parse(String(msg.data)) as ChatFrame);
        } catch (err) {
          console.error("bad event frame", err);
        }
      };
      return socket;
    }
  };
}
