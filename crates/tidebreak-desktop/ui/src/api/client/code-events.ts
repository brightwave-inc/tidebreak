import type {
  CodeApprovalSnapshot,
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
      const url = `${this.baseUrl.replace(/^http/, "ws")}/code/sessions/${encodeURIComponent(sessionId)}/events?after=${after}`;
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

    /** Open the install-wide digest channel; auth via Sec-WebSocket-Protocol. */
    openCodeUpdates(onNotice: (notice: CodeUpdateNotice) => void): WebSocket {
      const url = `${this.baseUrl.replace(/^http/, "ws")}/code/updates`;
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
            `/code/sessions/${encodeURIComponent(sessionId)}/attention`,
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
      const body = await this.json<unknown>(`/code/approvals${suffix}`, {
        headers: this.headers(),
      });
      return parseList(body, parseCodeApproval, "code approvals");
    }

    async decideCodeApproval(
      approvalId: string,
      body: { decision: "approve" | "deny"; feedback?: string },
    ): Promise<CodeApprovalSnapshot> {
      return requireParsed(
        parseCodeApproval(
          await this.json(
            `/code/approvals/${encodeURIComponent(approvalId)}/decision`,
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
