import { useEffect, useRef, useState } from "react";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

import type { ApiClient } from "../api/client";
import { Button } from "@/components/ui/button";
import { friendlyErrorMessage } from "@/lib/utils";
import { resolveShellShortcut, usesCommandModifier } from "../ShellShortcuts";

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

type TerminalClient = Pick<
  ApiClient,
  | "listCodeTerminals"
  | "createCodeTerminal"
  | "readCodeTerminal"
  | "writeCodeTerminal"
  | "resizeCodeTerminal"
>;

type RenderTerminalIdentity = {
  client: TerminalClient;
  workspaceId: string;
  requestedTerminalId: string | null;
  epoch: number;
};

type TerminalWriter = {
  token: number;
  client: TerminalClient;
  workspaceId: string;
  terminalId: string;
  chunks: string[];
  running: boolean;
  failed: boolean;
  failureMessage?: string;
  cancelled: boolean;
};

type ActiveTerminal = {
  epoch: number;
  writer: TerminalWriter;
};

type TerminalRecoveryError = {
  kind: "attach" | "read";
  message: string;
};

type TerminalWriteFailure = {
  writerToken: number;
  message: string;
  unsentBytes: number;
};

let nextWriterToken = 1;

/**
 * Ephemeral renderer over the cursor-pull terminal API.
 *
 * Created on open, disposed on close. Reopening re-fetches recent ring bytes
 * rather than keeping xterm state. Writes are chunked under a frame budget;
 * a stall surfaces a reconnect control.
 *
 * `terminalId` names the shell to draw. Without one the pane adopts the
 * workspace's first live shell, or starts one — the path a link written
 * before terminals were tabs still takes. Either way it reports the id it
 * settled on, so the tab above it can name the same shell from then on.
 */
export function TerminalPane({
  client,
  workspaceId,
  terminalId,
  onAttach,
  hideHeader = false,
}: {
  client: TerminalClient;
  workspaceId: string;
  /** The shell this pane draws. Absent means adopt one and report it. */
  terminalId?: string;
  /** The shell the pane settled on, when that is not the one it was given. */
  onAttach?: (terminalId: string) => void;
  /** The drawer already names this surface. */
  hideHeader?: boolean;
}) {
  const renderIdentityRef = useRef<RenderTerminalIdentity | null>(null);
  let renderIdentity = renderIdentityRef.current;
  const requestedTerminalId = terminalId ?? null;
  if (
    !renderIdentity ||
    renderIdentity.client !== client ||
    renderIdentity.workspaceId !== workspaceId ||
    renderIdentity.requestedTerminalId !== requestedTerminalId
  ) {
    renderIdentity = {
      client,
      workspaceId,
      requestedTerminalId,
      epoch: (renderIdentity?.epoch ?? 0) + 1,
    };
    renderIdentityRef.current = renderIdentity;
  }
  const identityEpoch = renderIdentity.epoch;

  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const tidRef = useRef<string | null>(null);
  const activeTerminalRef = useRef<ActiveTerminal | null>(null);
  const writerRef = useRef<TerminalWriter | null>(null);
  /** Set by reconnect, so the next attach starts a shell instead of reusing. */
  const startFreshRef = useRef(false);
  const onAttachRef = useRef(onAttach);
  onAttachRef.current = onAttach;
  const cursorRef = useRef(0);
  const pendingRef = useRef("");
  const rafRef = useRef<number | null>(null);
  const lastFlushRef = useRef(Date.now());
  const inputPausedRef = useRef(true);
  const stalledRef = useRef(false);
  const [generation, setGeneration] = useState(0);
  const [ended, setEnded] = useState(false);
  const [overflow, setOverflow] = useState(false);
  const [stalled, setStalled] = useState(false);
  /**
   * False until this renderer has heard from the shell at all — the window
   * that covers spawning a process and reading its first bytes.
   */
  const [attached, setAttached] = useState(false);
  const [inputPaused, setInputPaused] = useState(true);
  const [terminalError, setTerminalError] =
    useState<TerminalRecoveryError | null>(null);
  const [writeFailure, setWriteFailure] = useState<TerminalWriteFailure | null>(
    null,
  );

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const term = new Terminal({
      convertEol: true,
      fontSize: 13,
      cursorBlink: true,
      disableStdin: true,
      theme: readXtermTheme(),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    fit.fit();
    termRef.current = term;
    fitRef.current = fit;

    const dataSub = term.onData((data) => {
      const active = activeTerminalRef.current;
      if (
        !active ||
        active.epoch !== identityEpoch ||
        renderIdentityRef.current?.epoch !== identityEpoch ||
        writerRef.current !== active.writer ||
        inputPausedRef.current ||
        active.writer.failed ||
        active.writer.cancelled
      ) {
        return;
      }
      active.writer.chunks.push(data);
      drainWriter(active.writer);
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
      const active = activeTerminalRef.current;
      const tid =
        active?.epoch === identityEpoch ? active.writer.terminalId : null;
      const dims =
        term.cols && term.rows ? { cols: term.cols, rows: term.rows } : null;
      if (tid && dims) {
        void client.resizeCodeTerminal(workspaceId, tid, dims.cols, dims.rows);
      }
    };
    window.addEventListener("resize", onResize);
    const observer =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(onResize);
    observer?.observe(host);

    const root = document.documentElement;
    const themeObserver = new MutationObserver(() => {
      term.options.theme = readXtermTheme();
    });
    themeObserver.observe(root, {
      attributes: true,
      attributeFilter: ["class"],
    });

    return () => {
      themeObserver.disconnect();
      window.removeEventListener("resize", onResize);
      observer?.disconnect();
      host.removeEventListener("keydown", onKeyDownCapture, true);
      dataSub.dispose();
      if (rafRef.current != null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [client, workspaceId, terminalId, generation, identityEpoch]);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    setInputPausedForCurrent(true);
    setAttached(false);
    setEnded(false);
    setOverflow(false);
    setStalled(false);
    stalledRef.current = false;
    setTerminalError(null);
    pendingRef.current = "";
    cursorRef.current = 0;
    tidRef.current = null;
    activeTerminalRef.current = null;

    async function pull(): Promise<boolean> {
      const active = activeTerminalRef.current;
      if (
        !active ||
        active.epoch !== identityEpoch ||
        renderIdentityRef.current?.epoch !== identityEpoch
      ) {
        return false;
      }
      const tid = active.writer.terminalId;
      try {
        const page = await client.readCodeTerminal(
          workspaceId,
          tid,
          cursorRef.current,
        );
        if (
          cancelled ||
          activeTerminalRef.current !== active ||
          renderIdentityRef.current?.epoch !== identityEpoch
        ) {
          return false;
        }
        cursorRef.current = page.cursor;
        setAttached(true);
        setTerminalError(null);
        if (page.overflow) setOverflow(true);
        setEnded(page.ended);
        setInputPausedForCurrent(
          page.ended || active.writer.failed || stalledRef.current,
        );
        const text = decodeTerminalBytes(page.bytes);
        if (text.includes(TRUNCATION_TEXT)) setOverflow(true);
        if (text) enqueue(text, identityEpoch);
        return !page.ended;
      } catch (err) {
        if (
          !cancelled &&
          activeTerminalRef.current === active &&
          renderIdentityRef.current?.epoch === identityEpoch
        ) {
          setInputPausedForCurrent(true);
          setTerminalError({
            kind: "read",
            message: friendlyErrorMessage(err, "Could not read the terminal"),
          });
        }
        return false;
      }
    }

    function schedulePull() {
      timer = setTimeout(() => {
        void pull().then((keepPolling) => {
          if (keepPolling && !cancelled) schedulePull();
        });
      }, POLL_MS);
    }

    async function attach() {
      const cols = termRef.current?.cols;
      const rows = termRef.current?.rows;
      const size = { ...(cols ? { cols } : {}), ...(rows ? { rows } : {}) };
      try {
        // Reconnect always starts a fresh shell: it is the answer to one that
        // stalled or died, so re-attaching to the same id would land back on
        // the thing the reader asked to get away from.
        const start = startFreshRef.current;
        startFreshRef.current = false;
        const snap =
          !start && terminalId
            ? { id: terminalId, ended: false }
            : await adoptOrCreate(start);
        if (cancelled || !snap) return;
        tidRef.current = snap.id;
        cursorRef.current = 0;
        const writer = writerFor(snap.id);
        activeTerminalRef.current = { epoch: identityEpoch, writer };
        if (writer.failed) {
          surfaceWriteFailure(writer);
        } else {
          drainWriter(writer);
        }
        if (snap.ended) setEnded(true);
        if (snap.id !== terminalId) onAttachRef.current?.(snap.id);
        const keepPolling = await pull();
        if (keepPolling && !cancelled) schedulePull();
      } catch (err) {
        if (!cancelled && renderIdentityRef.current?.epoch === identityEpoch) {
          setInputPausedForCurrent(true);
          setTerminalError({
            kind: "attach",
            message: friendlyErrorMessage(err, "Could not open a terminal"),
          });
        }
      }

      async function adoptOrCreate(forceNew: boolean) {
        if (!forceNew) {
          const listed = await client.listCodeTerminals(workspaceId);
          if (cancelled || renderIdentityRef.current?.epoch !== identityEpoch) {
            return null;
          }
          const live = listed.find((item) => !item.ended);
          if (live) return live;
        }
        if (cancelled || renderIdentityRef.current?.epoch !== identityEpoch) {
          return null;
        }
        return client.createCodeTerminal(workspaceId, size);
      }

      function writerFor(tid: string): TerminalWriter {
        const existing = writerRef.current;
        if (
          existing &&
          !existing.cancelled &&
          existing.client === client &&
          existing.workspaceId === workspaceId &&
          existing.terminalId === tid
        ) {
          return existing;
        }
        abandonWriter(existing);
        const writer: TerminalWriter = {
          token: nextWriterToken,
          client,
          workspaceId,
          terminalId: tid,
          chunks: [],
          running: false,
          failed: false,
          cancelled: false,
        };
        nextWriterToken += 1;
        writerRef.current = writer;
        setWriteFailure(null);
        return writer;
      }
    }

    void attach();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
      const active = activeTerminalRef.current;
      if (active?.epoch === identityEpoch) {
        const nextIdentity = renderIdentityRef.current;
        const keepsActiveTerminal =
          nextIdentity?.client === client &&
          nextIdentity.workspaceId === workspaceId &&
          (nextIdentity.requestedTerminalId === requestedTerminalId ||
            nextIdentity.requestedTerminalId === active.writer.terminalId);
        if (!keepsActiveTerminal) {
          abandonWriter(active.writer);
          if (writerRef.current === active.writer) writerRef.current = null;
          setWriteFailure(null);
        }
        activeTerminalRef.current = null;
        tidRef.current = null;
      }
    };
  }, [client, workspaceId, terminalId, generation, identityEpoch]);

  useEffect(
    () => () => {
      abandonWriter(writerRef.current);
      writerRef.current = null;
      activeTerminalRef.current = null;
    },
    [],
  );

  useEffect(() => {
    const timer = setInterval(() => {
      if (
        pendingRef.current.length > FRAME_BUDGET * 8 &&
        Date.now() - lastFlushRef.current > STALL_MS
      ) {
        stalledRef.current = true;
        setStalled(true);
        setInputPausedForCurrent(true);
      }
    }, 500);
    return () => clearInterval(timer);
  }, [generation]);

  function enqueue(text: string, epoch: number) {
    if (renderIdentityRef.current?.epoch !== epoch) return;
    pendingRef.current += text;
    flushSoon(epoch);
  }

  function flushSoon(epoch: number) {
    if (rafRef.current != null) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      if (renderIdentityRef.current?.epoch !== epoch) return;
      const term = termRef.current;
      if (!term) return;
      const chunk = pendingRef.current.slice(0, FRAME_BUDGET);
      pendingRef.current = pendingRef.current.slice(FRAME_BUDGET);
      lastFlushRef.current = Date.now();
      term.write(chunk, () => {
        if (renderIdentityRef.current?.epoch !== epoch) return;
        if (pendingRef.current) flushSoon(epoch);
      });
    });
  }

  function drainWriter(writer: TerminalWriter) {
    if (
      writer.running ||
      writer.failed ||
      writer.cancelled ||
      !writerIsCurrent(writer)
    ) {
      return;
    }
    writer.running = true;
    void (async () => {
      try {
        while (
          !writer.failed &&
          !writer.cancelled &&
          writerIsCurrent(writer) &&
          writer.chunks.length
        ) {
          const chunk = writer.chunks[0];
          if (chunk == null) break;
          try {
            await writer.client.writeCodeTerminal(
              writer.workspaceId,
              writer.terminalId,
              chunk,
            );
          } catch (err) {
            if (writer.cancelled) break;
            writer.failed = true;
            writer.failureMessage = friendlyErrorMessage(
              err,
              "Could not send terminal input",
            );
            if (writerIsCurrent(writer)) surfaceWriteFailure(writer);
            break;
          }
          if (writer.cancelled) break;
          if (writer.chunks[0] === chunk) writer.chunks.shift();
        }
      } finally {
        writer.running = false;
        if (
          !writer.failed &&
          !writer.cancelled &&
          writerIsCurrent(writer) &&
          writer.chunks.length
        ) {
          drainWriter(writer);
        }
      }
    })();
  }

  function writerIsCurrent(writer: TerminalWriter) {
    const active = activeTerminalRef.current;
    return (
      writerRef.current === writer &&
      active?.writer === writer &&
      active.epoch === renderIdentityRef.current?.epoch
    );
  }

  function surfaceWriteFailure(writer: TerminalWriter) {
    if (!writer.failureMessage || !writerIsCurrent(writer)) return;
    setInputPausedForCurrent(true);
    setWriteFailure({
      writerToken: writer.token,
      message: writer.failureMessage,
      unsentBytes: unsentByteCount(writer.chunks),
    });
  }

  function setInputPausedForCurrent(paused: boolean) {
    inputPausedRef.current = paused;
    setInputPaused(paused);
    const term = termRef.current;
    if (term?.options) term.options.disableStdin = paused;
  }

  function retryTerminal() {
    setTerminalError(null);
    setAttached(false);
    setInputPausedForCurrent(true);
    advanceIdentityEpoch();
    setGeneration((value) => value + 1);
  }

  function retryUnsentInput() {
    const writer = writerRef.current;
    if (
      !writer ||
      writer.token !== writeFailure?.writerToken ||
      writer.cancelled
    ) {
      return;
    }
    writer.failed = false;
    writer.failureMessage = undefined;
    setWriteFailure(null);
    setInputPausedForCurrent(
      !attached || ended || stalledRef.current || terminalError != null,
    );
    drainWriter(writer);
  }

  function discardUnsentInput() {
    const writer = writerRef.current;
    if (!writer || writer.token !== writeFailure?.writerToken) return;
    writer.chunks.length = 0;
    writer.failed = false;
    writer.failureMessage = undefined;
    setWriteFailure(null);
    if (attached && !ended && !stalled && !terminalError) {
      setInputPausedForCurrent(false);
    }
  }

  function reconnect() {
    abandonWriter(writerRef.current);
    writerRef.current = null;
    activeTerminalRef.current = null;
    startFreshRef.current = true;
    stalledRef.current = false;
    setStalled(false);
    setEnded(false);
    setOverflow(false);
    setAttached(false);
    setTerminalError(null);
    setWriteFailure(null);
    setInputPausedForCurrent(true);
    pendingRef.current = "";
    tidRef.current = null;
    advanceIdentityEpoch();
    setGeneration((value) => value + 1);
  }

  function advanceIdentityEpoch() {
    const current = renderIdentityRef.current;
    if (!current) return;
    renderIdentityRef.current = { ...current, epoch: current.epoch + 1 };
  }

  function abandonWriter(writer: TerminalWriter | null) {
    if (!writer) return;
    writer.cancelled = true;
    writer.failed = false;
    writer.failureMessage = undefined;
    writer.chunks.length = 0;
  }

  const unsentLabel = writeFailure
    ? `${writeFailure.unsentBytes} unsent ${writeFailure.unsentBytes === 1 ? "byte" : "bytes"}`
    : null;

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
        <p
          className="text-muted-foreground px-3 py-2 text-xs"
          data-testid="terminal-truncated"
        >
          Output was truncated.
        </p>
      )}
      {ended && (
        <div
          className="flex items-center gap-2 px-3 py-2 text-xs"
          data-testid="terminal-ended"
        >
          <p className="text-muted-foreground">Shell ended.</p>
          <Button type="button" size="xs" onClick={reconnect}>
            Start a new shell
          </Button>
        </div>
      )}
      {stalled && (
        <div className="flex items-center gap-2 px-3 py-2 text-xs">
          <p>The terminal renderer stalled.</p>
          <Button type="button" size="xs" onClick={reconnect}>
            Reconnect
          </Button>
        </div>
      )}
      {writeFailure && (
        <div
          className="flex shrink-0 flex-col gap-2 border-b border-critical-border bg-critical-background px-3 py-2 sm:flex-row sm:items-center sm:justify-between"
          data-testid="terminal-write-failure"
          role="alert"
        >
          <div className="min-w-0">
            <p className="text-sm font-medium text-critical-foreground">
              Terminal input paused · {unsentLabel}
            </p>
            <p className="mt-0.5 text-xs text-critical-foreground-muted">
              {asSentence(writeFailure.message)} Retry sends the input to this
              shell and can repeat it if the first request arrived. Reconnect
              discards it and opens a new shell. Discard drops it and keeps this
              shell.
            </p>
          </div>
          <div className="flex shrink-0 flex-wrap items-center gap-1.5">
            <Button type="button" size="xs" onClick={retryUnsentInput}>
              Retry
            </Button>
            <Button
              type="button"
              size="xs"
              variant="outline"
              onClick={reconnect}
            >
              Reconnect
            </Button>
            <Button
              type="button"
              size="xs"
              variant="ghost-destructive"
              onClick={discardUnsentInput}
            >
              Discard
            </Button>
          </div>
        </div>
      )}
      {terminalError && (
        <div
          className="flex shrink-0 flex-col gap-2 border-b border-critical-border bg-critical-background px-3 py-2 sm:flex-row sm:items-center sm:justify-between"
          data-testid={`terminal-${terminalError.kind}-error`}
          role="alert"
        >
          <div className="min-w-0">
            <p className="text-sm font-medium text-critical-foreground">
              {terminalError.message}
            </p>
            <p className="mt-0.5 text-xs text-critical-foreground-muted">
              {terminalError.kind === "read"
                ? "Input is paused so you do not send commands without seeing the result."
                : "Input stays paused until the shell opens."}
            </p>
          </div>
          <Button
            type="button"
            size="xs"
            variant="outline"
            onClick={retryTerminal}
          >
            Retry
          </Button>
        </div>
      )}
      <div className="relative min-h-0 flex-1">
        <div
          ref={hostRef}
          className="h-full"
          data-testid="terminal-host"
          // xterm builds its own tree inside this host; the host itself is a
          // plain div, so it needs a role that takes a name before the label
          // reaches anyone.
          role="group"
          aria-label="Terminal output"
          aria-disabled={inputPaused}
        />
        {!attached && !terminalError && (
          // Spawning a shell and reading its first bytes takes a moment, and
          // an empty black rectangle is indistinguishable from one that
          // failed. The line sits over the host rather than above it so the
          // terminal does not jump a row when the output lands, and it goes
          // as soon as the shell answers — a live shell showing nothing is a
          // cleared screen, not a pending one.
          <p
            role="status"
            data-testid="terminal-starting"
            className="text-muted-foreground pointer-events-none absolute inset-x-3 top-2 text-xs"
          >
            Opening a shell in the worktree…
          </p>
        )}
      </div>
    </div>
  );
}

function unsentByteCount(chunks: string[]): number {
  return chunks.reduce(
    (total, chunk) => total + new TextEncoder().encode(chunk).byteLength,
    0,
  );
}

function asSentence(message: string): string {
  return /[.!?]$/.test(message) ? message : `${message}.`;
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
