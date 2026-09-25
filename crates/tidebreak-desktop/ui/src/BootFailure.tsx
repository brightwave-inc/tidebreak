import { useEffect, useState } from "react";
import { ExternalLink, Laptop, RotateCw } from "lucide-react";

import type { Attachment } from "./api";
import type { LocalBootFailure } from "./bootRecovery";
import { copyPlainText } from "./ClipboardCopyButton";
import { BootBrand } from "./Logomark";
import { openInBrowser } from "./openInBrowser";
import { WindowDragStrip } from "./WindowDragStrip";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { useRetry } from "@/components/ui/useRetry";
import { scrubLogText } from "./rendererErrors";
import { friendlyErrorMessage, sentenceStart } from "@/lib/utils";

/** Where a person gets the newest Tidebreak when this one is too old. */
export const LATEST_RELEASE_URL =
  "https://github.com/brightwave-inc/tidebreak/releases/latest";

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
 * the app. The error is worded for the reader on the screen; the raw error
 * rides in the copied debug report, where a bug report needs it.
 *
 * The server inside the app fails for a handful of known reasons, and each
 * gets its own sentence and the one action that helps: another process
 * holding the data folder, data from a newer version, a full disk. Try again
 * really runs the boot again, a server that stopped after it started offers
 * a restart, and the screen says where the conversations still are.
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
  /**
   * What the desktop knows about a local server that did not start: which
   * known failure it was, whether it stopped after starting, and where the
   * data folder is. `null` for a remote machine or outside the desktop.
   */
  local?: LocalBootFailure | null;
  onRetry: () => void | Promise<void>;
  onWorkLocally: () => Promise<void>;
  /** Quit and reopen Tidebreak, for a server that stopped after starting. */
  onRestart?: () => void | Promise<void>;
  /** Open the data folder in the file manager. */
  onRevealDataDir?: () => void | Promise<void>;
  onReportProblem?: () => void;
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
  // The raw error is the evidence a bug report needs, so it stays here,
  // scrubbed the way the renderer log scrubs it, and never on the screen.
  const error =
    input.error instanceof Error
      ? {
          name: input.error.name,
          message: scrubLogText(input.error.message, REPORT_MESSAGE_CHARS),
        }
      : {
          name: null,
          // raw-error-ok: the copied report keeps the raw value as evidence.
          message: scrubLogText(String(input.error), REPORT_MESSAGE_CHARS),
        };
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

/** The bound the renderer's own log puts on an error message. */
const REPORT_MESSAGE_CHARS = 1_000;

/** The remote machine's address, or `null` when this is a local boot. */
function attachedMachine(attachment: BootAttachment | null): string | null {
  if (!attachment || attachment.attachment !== "remote") return null;
  return attachment.baseUrl;
}

/** What the boot screen says, and the one action it leads with. */
export type BootFailureCopy = {
  headline: string;
  /** What happened and what to do, as one or two sentences. */
  body: string;
  /** The host's own words, for a failure the sentence cannot carry. */
  detail: string | null;
  /** Run boot again, restart the app, or get a newer version. */
  primary: "retry" | "restart" | "latest";
};

const LATEST_VERSION_STEP =
  "Install the latest version, then open Tidebreak again.";

/** The kind the host's error type puts before its message. */
const HOST_ERROR_KIND = /^(?:configuration|store|secret|server) error:\s*/i;

/**
 * The words for a failed boot. A remote machine and the catalog step keep
 * their headlines and the worded error; a local server that did not start
 * speaks for the known failure it hit.
 */
export function bootFailureCopy({
  stage,
  error,
  attachment,
  local = null,
}: {
  stage: BootStage;
  error: unknown;
  attachment: BootAttachment | null;
  local?: LocalBootFailure | null;
}): BootFailureCopy {
  // Worded for the reader, and set as a sentence: the host writes its
  // errors as log fragments ("another instance already owns this data
  // directory"). The raw error, in the machine's voice, goes in the copied
  // report.
  const worded = sentenceStart(
    friendlyErrorMessage(error, "No error message was recorded."),
  );
  const machine = attachedMachine(attachment);
  if (machine) {
    return {
      headline: `Could not reach ${machine}.`,
      body: worded,
      detail: null,
      primary: "retry",
    };
  }
  if (stage === "catalog") {
    return {
      headline: "Tidebreak started, but could not load its models.",
      body: worded,
      detail: null,
      primary: "retry",
    };
  }
  if (!local) {
    return {
      headline: "Tidebreak could not connect to its server.",
      body: worded,
      detail: null,
      primary: "retry",
    };
  }
  // The host's own words, without the error kind it prefixes them with
  // ("store error: …"), for the failures a sentence cannot carry.
  const hostWords = sentenceStart(
    friendlyErrorMessage(error, "No error message was recorded.").replace(
      HOST_ERROR_KIND,
      "",
    ),
  );
  if (local.stopped) {
    return {
      headline: "Tidebreak's server stopped.",
      body: "Restart Tidebreak to start it again.",
      detail: null,
      primary: "restart",
    };
  }
  switch (local.kind) {
    case "instance_lock":
      return {
        headline: "Another Tidebreak is using your data.",
        body: "Another Tidebreak process has your data folder open, such as a tidebreak command running in Terminal. Quit it, then try again.",
        detail: null,
        primary: "retry",
      };
    case "newer_version":
      return {
        headline: "Your data is from a newer version of Tidebreak.",
        body: `This version cannot open it. ${LATEST_VERSION_STEP}`,
        detail: null,
        primary: "latest",
      };
    case "unrecognized_data":
      return {
        headline: "Tidebreak does not recognize your data.",
        body: "It may come from a newer version of Tidebreak, or part of it may be damaged. Tidebreak changed nothing. Install the latest version, and report a problem if that does not help.",
        detail: null,
        primary: "latest",
      };
    case "unsupported_version":
      return {
        headline: "This version of Tidebreak cannot open your data.",
        body: `It was released before it could. ${LATEST_VERSION_STEP}`,
        detail: null,
        primary: "latest",
      };
    case "migration":
      return {
        headline: "Tidebreak could not update your data.",
        body: "Nothing is lost: Tidebreak copies your data before it updates it. Try again, and report a problem if it keeps failing.",
        detail: hostWords,
        primary: "retry",
      };
    case "disk_full":
      return {
        headline: "Your disk is full.",
        body: "Tidebreak needs free space to open your data. Free up space, then try again.",
        detail: null,
        primary: "retry",
      };
    case "keychain":
      return {
        headline: "Tidebreak could not read its saved credentials.",
        body: "Check that your login keychain is unlocked, then try again.",
        detail: null,
        primary: "retry",
      };
    case "unknown":
      return {
        headline: "Tidebreak could not start.",
        body: hostWords,
        detail: null,
        primary: "retry",
      };
  }
}

/**
 * The data folder as a person reads it: the home folder as `~`. The full
 * path stays in the tooltip.
 */
export function displayDataDir(path: string): string {
  return path.replace(/^\/(?:Users|home)\/[^/]+(?=\/|$)/, "~");
}

/** The file manager's name on this platform. */
function revealLabel(): string {
  return globalThis.navigator?.userAgent.includes("Mac OS")
    ? "Show in Finder"
    : "Show folder";
}

export function BootFailure({
  stage,
  error,
  attachment,
  appVersion,
  local = null,
  onRetry,
  onWorkLocally,
  onRestart,
  onRevealDataDir,
  onReportProblem,
  writeClipboard = copyPlainText,
}: BootFailureProps) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">(
    "idle",
  );
  const [detaching, setDetaching] = useState(false);
  const retry = useRetry(onRetry);
  const restart = useRetry(() => onRestart?.());
  const machine = attachedMachine(attachment);
  const copy = bootFailureCopy({ stage, error, attachment, local });
  // A remote machine's boot never reads this computer's data folder.
  const dataDir = machine ? null : (local?.dataDir ?? null);

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

  const busy = detaching || retry.pending || restart.pending;
  const primary =
    copy.primary === "restart" && onRestart ? (
      <Button size="sm" onClick={restart.retry} disabled={busy}>
        {restart.pending ? (
          <Spinner aria-hidden="true" className="text-current" />
        ) : (
          <RotateCw size={16} aria-hidden />
        )}
        Restart Tidebreak
      </Button>
    ) : copy.primary === "latest" ? (
      <Button
        size="sm"
        onClick={() => void openInBrowser(LATEST_RELEASE_URL)}
        disabled={busy}
      >
        Get the latest version
        <ExternalLink size={16} aria-hidden />
      </Button>
    ) : (
      <Button size="sm" onClick={retry.retry} disabled={busy}>
        {retry.pending ? (
          <Spinner aria-hidden="true" className="text-current" />
        ) : (
          <RotateCw size={16} aria-hidden />
        )}
        Try again
      </Button>
    );

  return (
    <div className="boot" role="alert">
      <WindowDragStrip />
      <BootBrand />
      <h1>{copy.headline}</h1>
      <p className="boot-message">{copy.body}</p>
      {copy.detail && <p className="boot-error-detail">{copy.detail}</p>}
      {machine && (
        <p className="boot-error-hint">
          Work on that machine keeps running. Returning to this computer changes
          nothing there.
        </p>
      )}
      {dataDir && (
        <p className="boot-error-hint">
          Your conversations are still on disk at{" "}
          {/* Whole, so the path never breaks at the space inside it. */}
          <span className="font-mono whitespace-nowrap" title={dataDir}>
            {displayDataDir(dataDir)}
          </span>
          .
          {onRevealDataDir && (
            <>
              {" "}
              <Button
                type="button"
                variant="link"
                size="2xs"
                className="inline h-auto border-0 p-0 align-baseline text-xs"
                onClick={() => void onRevealDataDir()}
              >
                {revealLabel()}
              </Button>
            </>
          )}
        </p>
      )}
      <div className="boot-actions">
        {primary}
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
        {onReportProblem && (
          <Button size="sm" variant="ghost" onClick={onReportProblem}>
            Report a problem…
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
