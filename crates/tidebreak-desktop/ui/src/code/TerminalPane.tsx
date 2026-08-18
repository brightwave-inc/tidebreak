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
 * xterm theme built from the live CSS tokens on `:root`.
 *
 * Status quads supply the ANSI hues. Magenta has no token of its own, so it
 * reuses the critical foreground; cyan reuses info.
 */
export type XtermTheme = {
  background: string;
  foreground: string;
  cursor: string;
  cursorAccent: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
};

export function readXtermTheme(
  style: CSSStyleDeclaration = getComputedStyle(document.documentElement),
): XtermTheme {
  const token = (name: string, fallback: string) =>
    resolveCssColor(style.getPropertyValue(name).trim() || fallback);

  const background = token("--background", "#18181b");
  const foreground = token("--foreground", "#e4e4e7");
  const muted = token("--muted-foreground", "#71717a");
  const success = token("--success", "#22c55e");
  const successFg = token("--success-foreground", "#86efac");
  const warning = token("--warning", "#eab308");
  const warningFg = token("--warning-foreground", "#fde047");
  const critical = token("--critical", "#ef4444");
  const criticalFg = token("--critical-foreground", "#fca5a5");
  const info = token("--info", "#3b82f6");
  const infoFg = token("--info-foreground", "#93c5fd");

  return {
    background,
    foreground,
    cursor: foreground,
    cursorAccent: background,
    black: muted,
    red: critical,
    green: success,
    yellow: warning,
    blue: info,
    magenta: criticalFg,
    cyan: infoFg,
    white: foreground,
    brightBlack: muted,
    brightRed: criticalFg,
    brightGreen: successFg,
    brightYellow: warningFg,
    brightBlue: infoFg,
    brightMagenta: criticalFg,
    brightCyan: infoFg,
    brightWhite: foreground,
  };
}

/**
 * Turn a CSS color (including `oklch(...)`) into something the xterm canvas
 * renderer can paint. A live element's computed `color` is the first try;
 * a 1×1 canvas `fillStyle` assignment is the fallback.
 */
export function resolveCssColor(value: string): string {
  if (!value) return value;
  const already = normalizeCssColor(value);
  if (already) return already;
  const computed = resolveViaComputedStyle(value);
  if (computed) return computed;
  const painted = resolveViaCanvas(value);
  if (painted) return painted;
  return value;
}

function resolveViaComputedStyle(value: string): string | null {
  if (typeof document === "undefined") return null;
  const probe = document.createElement("span");
  probe.style.color = value;
  document.documentElement.appendChild(probe);
  const computed = getComputedStyle(probe).color.trim();
  probe.remove();
  return normalizeCssColor(computed);
}

function resolveViaCanvas(value: string): string | null {
  if (typeof document === "undefined") return null;
  const canvas = document.createElement("canvas");
  canvas.width = 1;
  canvas.height = 1;
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  ctx.fillStyle = "#010101";
  ctx.fillStyle = value;
  const filled = String(ctx.fillStyle);
  if (filled === "#010101") return null;
  const fromFill = normalizeCssColor(filled);
  if (fromFill) return fromFill;
  // Modern syntax such as oklch() survives as the fillStyle string in
  // Chromium. Paint a pixel and read it back so xterm always gets #rrggbb.
  ctx.fillRect(0, 0, 1, 1);
  const pixel = ctx.getImageData(0, 0, 1, 1).data;
  if (pixel[3] === 0) return null;
  return toHex(String(pixel[0]), String(pixel[1]), String(pixel[2]));
}

function normalizeCssColor(color: string): string | null {
  if (!color) return null;
  if (color.startsWith("#") && (color.length === 7 || color.length === 4)) {
    return color;
  }
  const comma = color.match(
    /^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)(?:\s*,\s*[\d.]+\s*)?\)$/i,
  );
  if (comma) return toHex(comma[1], comma[2], comma[3]);
  const space = color.match(
    /^rgba?\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*[\d.%]+\s*)?\)$/i,
  );
  if (space) return toHex(space[1], space[2], space[3]);
  return null;
}

function toHex(r: string, g: string, b: string): string {
  const byte = (part: string) =>
    Math.max(0, Math.min(255, Math.round(Number(part))))
      .toString(16)
      .padStart(2, "0");
  return `#${byte(r)}${byte(g)}${byte(b)}`;
}

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
  hideHeader = false,
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
  /** The drawer already names this surface. */
  hideHeader?: boolean;
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
      theme: readXtermTheme(),
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
        // The terminal only exists inside a workspace, so the chords reaching
        // it are always code mode's.
        mode: "code",
      });
      if (!def) return;
      // The shell listener is on window capture, so it has already fired.
      // Stopping here keeps the chord out of xterm.
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
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(onResize);
    observer?.observe(host);

    const root = document.documentElement;
    const themeObserver = new MutationObserver(() => {
      term.options.theme = readXtermTheme();
    });
    themeObserver.observe(root, { attributes: true, attributeFilter: ["class"] });

    return () => {
      themeObserver.disconnect();
      window.removeEventListener("resize", onResize);
      observer?.disconnect();
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
      {!hideHeader && (
        <header className="flex shrink-0 items-center justify-between gap-2 border-b px-3 py-2">
          <h2 className="text-sm font-medium">Terminal</h2>
        </header>
      )}
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
      <div
        ref={hostRef}
        className="min-h-0 flex-1"
        data-testid="terminal-host"
        aria-label="Terminal output"
      />
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
