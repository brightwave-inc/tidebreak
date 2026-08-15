import { useEffect, useRef, useState } from "react";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

import type { ApiClient } from "../api/client";
import { Button } from "@/components/ui/button";
import { friendlyErrorMessage } from "@/lib/utils";
import {
  resolveShellShortcut,
  usesCommandModifier,
} from "../ShellShortcuts";

const POLL_MS = 200;
const FRAME_BUDGET = 8 * 1024;
const STALL_MS = 3_000;
const TRUNCATION_TEXT = "[output truncated]";

/**
 * Ephemeral renderer over the cursor-pull terminal API.
 *
 * Created on open, disposed on close. Reopening re-fetches recent ring bytes
 * rather than keeping xterm state. Writes are chunked under a frame budget;
 * a stall surfaces a reconnect control.
 */
export function TerminalPane({
  client,
  workspaceId,
}: {
  client: Pick<
    ApiClient,
    | "listCodeTerminals"
    | "createCodeTerminal"
    | "readCodeTerminal"
    | "writeCodeTerminal"
    | "resizeCodeTerminal"
  >;
  workspaceId: string;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const tidRef = useRef<string | null>(null);
  const cursorRef = useRef(0);
  const pendingRef = useRef("");
  const rafRef = useRef<number | null>(null);
  const lastFlushRef = useRef(Date.now());
  const [generation, setGeneration] = useState(0);
  const [ended, setEnded] = useState(false);
  const [overflow, setOverflow] = useState(false);
  const [stalled, setStalled] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const term = new Terminal({
      convertEol: true,
      fontSize: 13,
      cursorBlink: true,
      theme: {
        background: "#18181b",
        foreground: "#e4e4e7",
        cursor: "#e4e4e7",
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    fit.fit();
    termRef.current = term;
    fitRef.current = fit;

    const dataSub = term.onData((data) => {
      const tid = tidRef.current;
      if (!tid) return;
      void client.writeCodeTerminal(workspaceId, tid, data).catch(() => {
        // A failed keystroke is dropped; the next poll shows ended/error.
      });
    });
    const command = usesCommandModifier(navigator.userAgent);
    function onKeyDownCapture(event: KeyboardEvent) {
      const def = resolveShellShortcut(event, {
        editable: true,
        modalOpen: false,
        command,
      });
      if (!def) return;
      // Let the window listener fire the app shortcut; keep it out of xterm.
      event.stopPropagation();
    }
    host.addEventListener("keydown", onKeyDownCapture, true);

    const onResize = () => {
      fit.fit();
      const tid = tidRef.current;
      const dims = term.cols && term.rows ? { cols: term.cols, rows: term.rows } : null;
      if (tid && dims) {
        void client.resizeCodeTerminal(workspaceId, tid, dims.cols, dims.rows);
      }
    };
    window.addEventListener("resize", onResize);

    return () => {
      window.removeEventListener("resize", onResize);
      host.removeEventListener("keydown", onKeyDownCapture, true);
      dataSub.dispose();
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [client, workspaceId, generation]);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setInterval> | undefined;

    async function pull() {
      const tid = tidRef.current;
      if (!tid) return;
      try {
        const page = await client.readCodeTerminal(
          workspaceId,
          tid,
          cursorRef.current,
        );
        if (cancelled) return;
        cursorRef.current = page.cursor;
        if (page.overflow) setOverflow(true);
        if (page.ended) setEnded(true);
        const text = decodeTerminalBytes(page.bytes);
        if (text.includes(TRUNCATION_TEXT)) setOverflow(true);
        if (text) enqueue(text);
      } catch (err) {
        if (!cancelled) {
          setError(friendlyErrorMessage(err, "Could not read the terminal"));
        }
      }
    }

    async function attach() {
      try {
        const listed = await client.listCodeTerminals(workspaceId);
        const live = listed.find((item) => !item.ended);
        const cols = termRef.current?.cols;
        const rows = termRef.current?.rows;
        const snap =
          live ??
          (await client.createCodeTerminal(workspaceId, {
            ...(cols ? { cols } : {}),
            ...(rows ? { rows } : {}),
          }));
        if (cancelled) return;
        tidRef.current = snap.id;
        cursorRef.current = 0;
        if (snap.ended) setEnded(true);
        await pull();
        timer = setInterval(() => {
          void pull();
        }, POLL_MS);
      } catch (err) {
        if (!cancelled) {
          setError(friendlyErrorMessage(err, "Could not open a terminal"));
        }
      }
    }

    void attach();
    return () => {
      cancelled = true;
      if (timer) clearInterval(timer);
    };
  }, [client, workspaceId, generation]);

  useEffect(() => {
    const timer = setInterval(() => {
      if (
        pendingRef.current.length > FRAME_BUDGET * 8 &&
        Date.now() - lastFlushRef.current > STALL_MS
      ) {
        setStalled(true);
      }
    }, 500);
    return () => clearInterval(timer);
  }, [generation]);

  function enqueue(text: string) {
    pendingRef.current += text;
    flushSoon();
  }

  function flushSoon() {
    if (rafRef.current != null) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      const term = termRef.current;
      if (!term) return;
      const chunk = pendingRef.current.slice(0, FRAME_BUDGET);
      pendingRef.current = pendingRef.current.slice(FRAME_BUDGET);
      lastFlushRef.current = Date.now();
      term.write(chunk, () => {
        if (pendingRef.current) flushSoon();
      });
    });
  }

  function reconnect() {
    setStalled(false);
    setEnded(false);
    setOverflow(false);
    setError(null);
    pendingRef.current = "";
    tidRef.current = null;
    setGeneration((value) => value + 1);
  }

  return (
    <div
      className="flex min-h-0 flex-1 flex-col overflow-hidden"
      data-testid="terminal-pane"
    >
      <header className="flex shrink-0 items-center justify-between gap-2 border-b px-3 py-2">
        <h2 className="text-sm font-medium">Terminal</h2>
      </header>
      {overflow && (
        <p className="text-muted-foreground px-3 py-2 text-xs" data-testid="terminal-truncated">
          Output was truncated.
        </p>
      )}
      {ended && (
        <p className="text-muted-foreground px-3 py-2 text-xs" data-testid="terminal-ended">
          Shell ended.
        </p>
      )}
      {stalled && (
        <div className="flex items-center gap-2 px-3 py-2 text-xs">
          <p>The terminal renderer stalled.</p>
          <Button type="button" size="xs" onClick={reconnect}>
            Reconnect
          </Button>
        </div>
      )}
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      <div ref={hostRef} className="min-h-0 flex-1" data-testid="terminal-host" />
    </div>
  );
}

export function decodeTerminalBytes(encoded: string): string {
  if (!encoded) return "";
  try {
    const binary = atob(encoded);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) {
      bytes[i] = binary.charCodeAt(i);
    }
    return new TextDecoder().decode(bytes);
  } catch {
    return "";
  }
}

export function encodeTerminalBytes(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}
