import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { House, RotateCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { friendlyErrorMessage } from "@/lib/utils";
import { copyPlainText } from "./ClipboardCopyButton";
import { hasNativeHost } from "./host";
import { BootBrand } from "./Logomark";
import { scrubLogText } from "./rendererErrors";
import { WindowDragStrip } from "./WindowDragStrip";

/**
 * Stated next to the copy control, so a reader knows what a pasted report
 * carries before they paste it anywhere.
 */
export const CRASH_DEBUG_CONTENTS_NOTICE =
  "Includes the app version, the page, and the error. No credentials.";

/** The page as a path, without the query: search params stay out of reports. */
export function currentRoutePath(): string {
  const hash = globalThis.location?.hash ?? "";
  const path = hash.replace(/^#/, "").split("?")[0];
  return path || "/";
}

/** The bounds the renderer's own log uses for the same fields. */
const REPORT_MESSAGE_CHARS = 1_000;
const REPORT_STACK_CHARS = 8_000;
const REPORT_ROUTE_CHARS = 500;

/**
 * Routes whose parameter is a one-time capability rather than an address: the
 * nonce a channel's connect card carries, and the id an admin's workspace
 * grant link carries. Either one in a pasted report would let a reader act
 * on it.
 */
const CAPABILITY_ROUTES = [
  /^(\/connect\/)[^/]+/,
  /^(\/workspace-grant\/)[^/]+/,
];

/** A route as a report may carry it: capabilities and credentials removed. */
export function reportRoutePath(path: string): string {
  let redacted = path;
  for (const route of CAPABILITY_ROUTES) {
    redacted = redacted.replace(route, "$1[redacted]");
  }
  return scrubLogText(redacted, REPORT_ROUTE_CHARS);
}

/** Code mode has its own home; everywhere else, home is the Work home. */
export function homePathFor(pathname: string): "/" | "/code" {
  return pathname === "/code" || pathname.startsWith("/code/") ? "/code" : "/";
}

/**
 * The diagnostic payload for a crash, as a formatted JSON document.
 *
 * Built from named fields, like the boot screen's report, so nothing carrying
 * a token can be spread into it by accident. The message, stacks, and route
 * go through the same scrub the renderer's log uses (`scrubLogText`), so a
 * token inside an error, a URL's userinfo or query, or a connect nonce in the
 * route never reaches the clipboard: the notice beside the control promises
 * "No credentials."
 */
export function crashDebugReport(input: {
  error: unknown;
  componentStack?: string | null;
  route: string;
  appVersion: string | null;
  capturedAt: string;
  userAgent: string | null;
}): string {
  const error =
    input.error instanceof Error
      ? {
          name: input.error.name,
          message: scrubLogText(input.error.message, REPORT_MESSAGE_CHARS),
          stack: input.error.stack
            ? scrubLogText(input.error.stack, REPORT_STACK_CHARS)
            : null,
        }
      : {
          name: null,
          message: scrubLogText(
            friendlyErrorMessage(input.error, "No message"),
            REPORT_MESSAGE_CHARS,
          ),
          stack: null,
        };
  const componentStack = input.componentStack?.trim();
  return JSON.stringify(
    {
      capturedAt: input.capturedAt,
      appVersion: input.appVersion,
      route: reportRoutePath(input.route),
      error,
      componentStack: componentStack
        ? scrubLogText(componentStack, REPORT_STACK_CHARS)
        : null,
      userAgent: input.userAgent,
    },
    null,
    2,
  );
}

async function appVersion(): Promise<string | null> {
  if (!hasNativeHost()) return null;
  return getVersion().catch(() => null);
}

type CopyState = "idle" | "copied" | "failed";

/**
 * Copies a crash report and says whether it worked, the way the boot screen's
 * control does.
 */
export function useCopyCrashReport({
  error,
  componentStack,
  writeClipboard = copyPlainText,
}: {
  error: unknown;
  componentStack?: string | null;
  writeClipboard?: (text: string) => Promise<void>;
}) {
  const [copyState, setCopyState] = useState<CopyState>("idle");

  useEffect(() => {
    if (copyState === "idle") return;
    const timer = window.setTimeout(() => setCopyState("idle"), 3_000);
    return () => window.clearTimeout(timer);
  }, [copyState]);

  async function copy() {
    try {
      await writeClipboard(
        crashDebugReport({
          error,
          componentStack,
          route: currentRoutePath(),
          appVersion: await appVersion(),
          capturedAt: new Date().toISOString(),
          userAgent: globalThis.navigator?.userAgent ?? null,
        }),
      );
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
  }

  const label =
    copyState === "copied"
      ? "Copied"
      : copyState === "failed"
        ? "Copy failed"
        : "Copy debug info";
  return { copy, label };
}

export type CrashScreenProps = {
  error: unknown;
  componentStack?: string | null;
  onReload: () => void;
  onGoHome: () => void;
  /** Injectable for tests; defaults to the real clipboard. */
  writeClipboard?: (text: string) => Promise<void>;
};

/**
 * The full-window screen for a crash nothing closer could contain.
 *
 * Conversations and workspaces live in the server, not in this window, so a
 * reload is a safe recovery and the screen leads with it.
 */
export function CrashScreen({
  error,
  componentStack,
  onReload,
  onGoHome,
  writeClipboard,
}: CrashScreenProps) {
  const report = useCopyCrashReport({ error, componentStack, writeClipboard });
  return (
    // A layout that crashes leaves the screen inside the shell's flex row, so
    // it claims the whole row rather than sizing to its copy.
    <div className="boot w-full min-w-0 flex-1" role="alert">
      <WindowDragStrip />
      <BootBrand />
      <h1>Tidebreak hit an unexpected error.</h1>
      <p>Your conversations are saved. Reload the window to continue.</p>
      <p className="boot-error-detail">
        {friendlyErrorMessage(error, "No error message was recorded.")}
      </p>
      <div className="boot-actions">
        <Button size="sm" onClick={onReload}>
          <RotateCw aria-hidden="true" />
          Reload
        </Button>
        <Button size="sm" variant="outline" onClick={onGoHome}>
          <House aria-hidden="true" />
          Go home
        </Button>
        <Button size="sm" variant="ghost" onClick={() => void report.copy()}>
          {report.label}
        </Button>
      </div>
      <p className="boot-error-hint">{CRASH_DEBUG_CONTENTS_NOTICE}</p>
    </div>
  );
}
