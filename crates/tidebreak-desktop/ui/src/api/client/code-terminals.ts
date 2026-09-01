import type { CodeTerminalRead, CodeTerminalSnapshot } from "../types";
import {
  parseCodeTerminal,
  parseCodeTerminalList,
  parseCodeTerminalRead,
} from "../../code/parsers";
import { type Constructor, HttpCore, requireParsed } from "./http";

function encodeUtf8Base64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

/** Workspace terminals. */
export function withCodeTerminalsApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    async listCodeTerminals(
      workspaceId: string,
    ): Promise<CodeTerminalSnapshot[]> {
      const body = await this.json<unknown>(
        `/code/workspaces/${encodeURIComponent(workspaceId)}/terminals`,
        { headers: this.headers() },
      );
      return requireParsed(parseCodeTerminalList(body), "code terminals");
    }

    async createCodeTerminal(
      workspaceId: string,
      body: { cols?: number; rows?: number } = {},
    ): Promise<CodeTerminalSnapshot> {
      return requireParsed(
        parseCodeTerminal(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/terminals`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(body),
            },
          ),
        ),
        "code terminal",
      );
    }

    deleteCodeTerminal(workspaceId: string, terminalId: string): Promise<void> {
      return this.json(
        `/code/workspaces/${encodeURIComponent(workspaceId)}/terminals/${encodeURIComponent(terminalId)}`,
        { method: "DELETE", headers: this.headers() },
      );
    }

    async readCodeTerminal(
      workspaceId: string,
      terminalId: string,
      cursor = 0,
    ): Promise<CodeTerminalRead> {
      return requireParsed(
        parseCodeTerminalRead(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/terminals/${encodeURIComponent(terminalId)}/read?cursor=${encodeURIComponent(String(cursor))}`,
            { headers: this.headers() },
          ),
        ),
        "code terminal read",
      );
    }

    writeCodeTerminal(
      workspaceId: string,
      terminalId: string,
      data: string,
    ): Promise<void> {
      return this.json(
        `/code/workspaces/${encodeURIComponent(workspaceId)}/terminals/${encodeURIComponent(terminalId)}/write`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify({ bytes: encodeUtf8Base64(data) }),
        },
      );
    }

    async resizeCodeTerminal(
      workspaceId: string,
      terminalId: string,
      cols: number,
      rows: number,
    ): Promise<CodeTerminalSnapshot> {
      return requireParsed(
        parseCodeTerminal(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/terminals/${encodeURIComponent(terminalId)}/resize`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify({ cols, rows }),
            },
          ),
        ),
        "code terminal",
      );
    }
  };
}
