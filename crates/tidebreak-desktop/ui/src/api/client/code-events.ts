import type {
  CodeApprovalSnapshot,
  CodeApprovalDecisionBody,
  CodeSessionSnapshot,
  CodeUpdateNotice,
  SequencedCodeEventFrame,
} from "../types";
import {
  type Constructor,
  HttpCore,
  parseList,
  requireParsed,
  WS_HANDSHAKE,
  WS_TOKEN_PREFIX,
} from "./http";
import {
  parseCodeApproval,
  parseCodeSession,
  parseCodeUpdateNotice,
  parseSequencedCodeEvent,
} from "../../code/parsers";

/** Code event and update sockets, attention, and code approvals. */
export function withCodeEventsApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    /** Open the per-session journal; auth via Sec-WebSocket-Protocol. */
    openCodeEvents(
      sessionId: string,
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ): WebSocket {
      const url = `${this.baseUrl.replace(/^http/, "ws")}/sessions/${encodeURIComponent(sessionId)}/events?after=${after}`;
      const protocols = [WS_HANDSHAKE, `${WS_TOKEN_PREFIX}${this.token}`];
      const socket = new WebSocket(url, protocols);
      socket.onmessage = (msg) => {
        try {
          const frame = parseSequencedCodeEvent(JSON.parse(String(msg.data)));
          if (frame) onFrame(frame);
          else console.error("dropping malformed code event frame");
        } catch (err) {
          console.error("bad code event frame", err);
        }
      };
      return socket;
    }

    /**
     * One window of a session's journal: the newest `limit` events below
     * `before`, oldest first, the way the socket replays them. For a part of
     * a long session the socket no longer replays; the first frame carries
     * `truncated` when older events were left out.
     */
    async listCodeJournal(
      sessionId: string,
      window: { before: number; limit?: number },
      signal?: AbortSignal,
    ): Promise<SequencedCodeEventFrame[]> {
      const params = new URLSearchParams({ before: String(window.before) });
      if (window.limit !== undefined) params.set("limit", String(window.limit));
      return parseList(
        await this.json(
          `/sessions/${encodeURIComponent(sessionId)}/journal?${params}`,
          { headers: this.headers(), signal },
        ),
        parseSequencedCodeEvent,
        "code session journal",
      );
    }

    /** Open the install-wide digest channel; auth via Sec-WebSocket-Protocol. */
    openCodeUpdates(onNotice: (notice: CodeUpdateNotice) => void): WebSocket {
      const url = `${this.baseUrl.replace(/^http/, "ws")}/updates`;
      const protocols = [WS_HANDSHAKE, `${WS_TOKEN_PREFIX}${this.token}`];
      const socket = new WebSocket(url, protocols);
      socket.onmessage = (msg) => {
        try {
          const notice = parseCodeUpdateNotice(JSON.parse(String(msg.data)));
          if (notice) onNotice(notice);
          else console.error("dropping malformed code update notice");
        } catch (err) {
          console.error("bad code update notice", err);
        }
      };
      return socket;
    }

    async setCodeAttention(
      sessionId: string,
      body: { clear?: boolean; note?: string },
    ): Promise<CodeSessionSnapshot> {
      return requireParsed(
        parseCodeSession(
          await this.json(
            `/sessions/${encodeURIComponent(sessionId)}/attention`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(body),
            },
          ),
        ),
        "code attention",
      );
    }

    async listCodeApprovals(query?: {
      state?: "pending" | "approved" | "denied";
      sessionId?: string;
    }): Promise<CodeApprovalSnapshot[]> {
      const params = new URLSearchParams();
      if (query?.state) params.set("state", query.state);
      if (query?.sessionId) params.set("session_id", query.sessionId);
      const suffix = params.size > 0 ? `?${params}` : "";
      const body = await this.json<unknown>(`/approvals${suffix}`, {
        headers: this.headers(),
      });
      return parseList(body, parseCodeApproval, "code approvals");
    }

    async decideCodeApproval(
      approvalId: string,
      body: CodeApprovalDecisionBody,
    ): Promise<CodeApprovalSnapshot> {
      return requireParsed(
        parseCodeApproval(
          await this.json(
            `/approvals/${encodeURIComponent(approvalId)}/decision`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(body),
            },
          ),
        ),
        "code approval",
      );
    }
  };
}
