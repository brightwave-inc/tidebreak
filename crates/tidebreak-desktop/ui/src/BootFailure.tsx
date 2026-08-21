import { useEffect, useState } from "react";
import { Laptop, RotateCw } from "lucide-react";

import type { Attachment } from "./api";
import { copyPlainText } from "./ClipboardCopyButton";
import { Logomark } from "./Logomark";
import { WindowDragStrip } from "./WindowDragStrip";
import { Button } from "@/components/ui/button";

/**
 * The screen a reader lands on when the shell cannot reach the API it is
 * attached to.
 *
 * This used to be the logomark and the raw caught string, and nothing else. On
 * a remote attachment that is a locked door: the address lives in the shell,
 * the only command that forgets it is behind Settings, and Settings is behind
 * the very client that failed to boot. A transient outage on the other machine
 * therefore cost the reader the whole app until someone edited
 * `remote-machine.json` by hand.
 *
 * So the screen carries the three things the reader needs: which machine could
 * not be reached, a way to try again, and — when the answer is "that machine is
 * not coming back right now" — a way to return this window to the server inside
 * the app. The raw error stays, under the copy the reader can act on.
 */

/** Which step of boot failed. */
export type BootStage = "connect" | "catalog";

/** What the shell knew about its attachment when boot failed. */
export type BootAttachment = {
  attachment: Attachment;
  baseUrl: string | null;
  /**
   * Whether the bearer is a Gateway-minted one. `null` before the connection
   * resolves: the shell's address file records the machine, not how this
   * window ends up authenticating to it, and a debug report that guessed
   * `false` there would send a reader looking in the wrong place.
   */
  gatewayAuth: boolean | null;
};

export type BootFailureProps = {
  stage: BootStage;
  error: unknown;
  attachment: BootAttachment | null;
  appVersion: string | null;
  onRetry: () => void;
  onWorkLocally: () => Promise<void>;
  /** Injectable for tests; defaults to the real clipboard. */
  writeClipboard?: (text: string) => Promise<void>;
};

/**
 * Stated next to the copy control. The payload names the machine this window
 * is attached to and carries the failure verbatim; a reader pasting that into
 * an issue should know what they are pasting before they paste it.
 */
export const BOOT_DEBUG_CONTENTS_NOTICE =
  "Includes the machine address, the app version, and the error. No credentials.";

/**
 * The diagnostic payload, as a formatted JSON document.
 *
 * Deliberately built from named fields rather than from the objects they came
 * out of: `ServerInfo` carries the bearer token this window would have used,
 * and the whole value of a copy control on a boot screen is that a reader can
 * paste it somewhere public. Widening this to spread an existing object would
 * put the token one refactor away from the clipboard.
 */
export function bootDebugReport(input: {
  stage: BootStage;
  error: unknown;
  attachment: BootAttachment | null;
  appVersion: string | null;
  capturedAt: string;
  userAgent: string | null;
}): string {
  const error =
    input.error instanceof Error
      ? { name: input.error.name, message: input.error.message }
      : { name: null, message: String(input.error) };
  return JSON.stringify(
    {
      capturedAt: input.capturedAt,
      appVersion: input.appVersion,
      stage: input.stage,
      attachment: input.attachment?.attachment ?? null,
      remoteBaseUrl: input.attachment?.baseUrl ?? null,
      gatewayAuth: input.attachment?.gatewayAuth ?? null,
      error,
      userAgent: input.userAgent,
    },
    null,
    2,
  );
}

/** The remote machine's address, or `null` when this is a local boot. */
function attachedMachine(attachment: BootAttachment | null): string | null {
  if (!attachment || attachment.attachment !== "remote") return null;
  return attachment.baseUrl;
}

function headline(attachment: BootAttachment | null, stage: BootStage): string {
  const machine = attachedMachine(attachment);
  if (machine) return `Could not reach ${machine}.`;
  return stage === "connect"
    ? "Tidebreak could not start its server."
    : "Tidebreak started, but could not load its models.";
}

export function BootFailure({
  stage,
  error,
  attachment,
  appVersion,
  onRetry,
  onWorkLocally,
  writeClipboard = copyPlainText,
}: BootFailureProps) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");
  const [detaching, setDetaching] = useState(false);
  const machine = attachedMachine(attachment);

  useEffect(() => {
    if (copyState === "idle") return;
    const timer = window.setTimeout(() => setCopyState("idle"), 3_000);
    return () => window.clearTimeout(timer);
  }, [copyState]);

  async function onCopy() {
    try {
      await writeClipboard(
        bootDebugReport({
          stage,
          error,
          attachment,
          appVersion,
          capturedAt: new Date().toISOString(),
          userAgent: globalThis.navigator?.userAgent ?? null,
        }),
      );
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
  }

  async function onDetach() {
    setDetaching(true);
    try {
      await onWorkLocally();
    } finally {
      setDetaching(false);
    }
  }

  return (
    <div className="boot" role="alert">
      <WindowDragStrip />
      <div className="boot-brand">
        <Logomark />
        <h1>Tidebreak</h1>
      </div>
      <p>{headline(attachment, stage)}</p>
      <p className="boot-error-detail">{String(error)}</p>
      {machine && (
        <p className="boot-error-hint">
          Work on that machine keeps running. Returning to this computer changes
          nothing there.
        </p>
      )}
      <div className="boot-actions">
        <Button size="sm" onClick={onRetry} disabled={detaching}>
          <RotateCw size={16} aria-hidden />
          Try again
        </Button>
        {machine && (
          <Button
            size="sm"
            variant="outline"
            onClick={() => void onDetach()}
            disabled={detaching}
          >
            <Laptop size={16} aria-hidden />
            Work on this computer
          </Button>
        )}
        <Button size="sm" variant="ghost" onClick={() => void onCopy()}>
          {copyState === "copied"
            ? "Copied"
            : copyState === "failed"
              ? "Copy failed"
              : "Copy debug info"}
        </Button>
      </div>
      <p className="boot-error-hint">{BOOT_DEBUG_CONTENTS_NOTICE}</p>
    </div>
  );
}
