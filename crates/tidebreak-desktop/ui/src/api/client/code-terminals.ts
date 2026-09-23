import type {
  CodeTerminalRead,
  CodeTerminalSnapshot,
  HarnessKind,
  HarnessSignInRead,
  HarnessSignInTerminal,
} from "../types";
import {
  parseCodeTerminal,
  parseCodeTerminalList,
  parseCodeTerminalRead,
  parseHarnessSignInRead,
  parseHarnessSignInTerminal,
} from "../../code/parsers";
import { type Constructor, HttpCore, requireParsed } from "./http";

function encodeUtf8Base64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function signInPath(kind: HarnessKind, terminalId?: string): string {
  const base = `/code/harnesses/${encodeURIComponent(kind)}/sign-in`;
  return terminalId ? `${base}/${encodeURIComponent(terminalId)}` : base;
}

/** Workspace terminals, and the terminal an engine's sign-in runs in. */
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

    /**
     * Run an engine's own sign-in command (`claude auth login`) in a
     * terminal, or rejoin the one already running.
     */
    async startHarnessSignIn(
      kind: HarnessKind,
      body: { cols?: number; rows?: number } = {},
    ): Promise<HarnessSignInTerminal> {
      return requireParsed(
        parseHarnessSignInTerminal(
          await this.json(signInPath(kind), {
            method: "POST",
            headers: this.headers(true),
            body: JSON.stringify(body),
          }),
        ),
        "engine sign-in",
      );
    }

    async readHarnessSignIn(
      kind: HarnessKind,
      terminalId: string,
      cursor = 0,
    ): Promise<HarnessSignInRead> {
      return requireParsed(
        parseHarnessSignInRead(
          await this.json(
            `${signInPath(kind, terminalId)}/read?cursor=${encodeURIComponent(String(cursor))}`,
            { headers: this.headers() },
          ),
        ),
        "engine sign-in read",
      );
    }

    writeHarnessSignIn(
      kind: HarnessKind,
      terminalId: string,
      data: string,
    ): Promise<void> {
      return this.json(`${signInPath(kind, terminalId)}/write`, {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({ bytes: encodeUtf8Base64(data) }),
      });
    }

    async resizeHarnessSignIn(
      kind: HarnessKind,
      terminalId: string,
      cols: number,
      rows: number,
    ): Promise<HarnessSignInTerminal> {
      return requireParsed(
        parseHarnessSignInTerminal(
          await this.json(`${signInPath(kind, terminalId)}/resize`, {
            method: "POST",
            headers: this.headers(true),
            body: JSON.stringify({ cols, rows }),
          }),
        ),
        "engine sign-in",
      );
    }

    /** Stop the sign-in command, finished or not. */
    closeHarnessSignIn(kind: HarnessKind, terminalId: string): Promise<void> {
      return this.json(signInPath(kind, terminalId), {
        method: "DELETE",
        headers: this.headers(),
      });
    }
  };
}
