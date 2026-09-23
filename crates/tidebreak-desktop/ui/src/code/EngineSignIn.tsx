import { type ReactNode, useMemo, useRef, useState } from "react";
import { CircleCheck, LogIn, TriangleAlert } from "lucide-react";
import { create } from "zustand";

import type { ApiClient } from "../api/client";
import type {
  CodeTerminalSnapshot,
  HarnessDoctorEntry,
  HarnessKind,
  HarnessSignInTerminal,
} from "../api/types";
import { useApp } from "@/AppContext";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Spinner } from "@/components/ui/spinner";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { HARNESS_LABELS, harnessNeedsSignIn, isHarnessReady } from "./labels";
import { STATUS_TEXT } from "./statusTone";
import { TerminalPane, type TerminalClient } from "./TerminalPane";

/**
 * Engine sign-in: the engine's own login command, run inside Tidebreak.
 *
 * Tidebreak drives the engine binaries it downloaded (decision 41), and those
 * are not on the reader's `PATH`. "Sign in from your terminal" sent a person
 * who pressed Download to a command they did not have. Sign in runs the
 * pinned binary's own command (`claude auth login`) in a terminal the reader
 * types into, then checks the sign-in again when the command exits.
 *
 * One dialog serves every surface that offers the action: the doctor on Code
 * home and in Settings, the new-workspace picker, and a failed turn. Each
 * asks through [`openEngineSignIn`], and the host the app shell mounts draws
 * it.
 */

type EngineSignInStore = {
  /** The engine whose sign-in is open, if any. */
  kind: HarnessKind | null;
  /**
   * Counts finished re-checks, so a surface that holds its own copy of the
   * doctor report knows to read it again.
   */
  checks: number;
  open: (kind: HarnessKind) => void;
  close: () => void;
  recordCheck: () => void;
};

export const useEngineSignInStore = create<EngineSignInStore>((set) => ({
  kind: null,
  checks: 0,
  open: (kind) => set({ kind }),
  close: () => set({ kind: null }),
  recordCheck: () => set((state) => ({ checks: state.checks + 1 })),
}));

/** Open the sign-in for one engine, from any surface. */
export function openEngineSignIn(kind: HarnessKind): void {
  useEngineSignInStore.getState().open(kind);
}

/** What the dialog asks of the server. */
export type EngineSignInClient = Pick<
  ApiClient,
  | "startHarnessSignIn"
  | "readHarnessSignIn"
  | "writeHarnessSignIn"
  | "resizeHarnessSignIn"
  | "closeHarnessSignIn"
>;

/**
 * The workspace terminal's client, pointed at one engine's sign-in.
 *
 * `TerminalPane` knows terminals by workspace. A sign-in has none, so the
 * workspace id the pane passes is ignored and every call goes to the sign-in
 * routes. Starting a sign-in already rejoins one that is running, so there
 * is nothing to list first.
 */
function signInTerminalClient(
  client: EngineSignInClient,
  kind: HarnessKind,
): TerminalClient {
  const scope = `sign-in:${kind}`;
  const snapshot = (terminal: HarnessSignInTerminal): CodeTerminalSnapshot => ({
    id: terminal.id,
    workspace_id: scope,
    cols: terminal.cols,
    rows: terminal.rows,
    ended: terminal.ended,
    created_at: terminal.created_at,
  });
  return {
    listCodeTerminals: async () => [],
    createCodeTerminal: async (_workspace, size) =>
      snapshot(await client.startHarnessSignIn(kind, size)),
    readCodeTerminal: async (_workspace, terminalId, cursor) => {
      const page = await client.readHarnessSignIn(kind, terminalId, cursor);
      return {
        id: page.id,
        workspace_id: scope,
        bytes: page.bytes,
        cursor: page.cursor,
        overflow: page.overflow,
        truncated: page.truncated,
        ended: page.ended,
      };
    },
    writeCodeTerminal: (_workspace, terminalId, data) =>
      client.writeHarnessSignIn(kind, terminalId, data),
    resizeCodeTerminal: async (_workspace, terminalId, cols, rows) =>
      snapshot(await client.resizeHarnessSignIn(kind, terminalId, cols, rows)),
  };
}

type Phase =
  | { step: "running" }
  | { step: "checking" }
  | { step: "checked" }
  | { step: "check_failed"; message: string };

/**
 * The sign-in dialog: the engine's command in a terminal, and what the
 * re-check found once it exits.
 *
 * `entry` is the engine's row in the latest doctor report. `onRecheck`
 * reads that report again; the dialog reports from `entry` once it lands.
 */
export function EngineSignInDialog({
  client,
  kind,
  entry,
  open,
  onOpenChange,
  onRecheck,
}: {
  client: EngineSignInClient;
  kind: HarnessKind;
  entry: HarnessDoctorEntry | undefined;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onRecheck: () => Promise<void>;
}) {
  const label = HARNESS_LABELS[kind];
  const command = entry?.sign_in_command;
  const terminalClient = useMemo(
    () => signInTerminalClient(client, kind),
    [client, kind],
  );
  const [attempt, setAttempt] = useState(0);
  const [phase, setPhase] = useState<Phase>({ step: "running" });
  const terminalId = useRef<string | null>(null);

  async function recheck() {
    setPhase({ step: "checking" });
    try {
      await onRecheck();
      setPhase({ step: "checked" });
    } catch (err) {
      setPhase({
        step: "check_failed",
        message: friendlyErrorMessage(err, "Could not check the sign-in"),
      });
    }
  }

  function runAgain() {
    terminalId.current = null;
    setPhase({ step: "running" });
    setAttempt((value) => value + 1);
  }

  function close() {
    const running = phase.step === "running";
    const id = terminalId.current;
    terminalId.current = null;
    // Closing stops the command whether or not it finished. One still
    // running may have written its credentials already, so check anyway.
    if (id) void client.closeHarnessSignIn(kind, id).catch(() => {});
    if (running) void onRecheck().catch(() => {});
    onOpenChange(false);
  }

  const signedIn = Boolean(entry && isHarnessReady(entry));

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) close();
      }}
    >
      <DialogContent className="flex max-h-[90vh] max-w-3xl flex-col gap-4">
        <DialogHeader>
          <DialogTitle>Sign in to {label}</DialogTitle>
          <DialogDescription>
            {command ? (
              <>
                This runs{" "}
                <code className="text-foreground font-mono">{command}</code>{" "}
                with the copy of {label} that Tidebreak runs. Follow the prompts
                below. Your browser may open to finish.
              </>
            ) : (
              <>
                This runs the {label} sign-in with the copy Tidebreak runs.
                Follow the prompts below.
              </>
            )}
          </DialogDescription>
        </DialogHeader>
        <div
          className="border-border bg-background flex h-80 min-h-0 flex-col overflow-hidden rounded-md border py-1.5 pl-2.5 pr-1"
          data-testid="engine-sign-in-terminal"
        >
          <TerminalPane
            key={attempt}
            client={terminalClient}
            workspaceId={`sign-in:${kind}`}
            hideHeader
            showEndedNotice={false}
            startingText={
              command ? `Starting ${command}…` : "Starting the sign-in…"
            }
            onAttach={(id) => {
              terminalId.current = id;
            }}
            onEnded={() => void recheck()}
          />
        </div>
        <SignInStatus
          label={label}
          phase={phase}
          signedIn={signedIn}
          unconfirmed={entry?.authenticated === undefined}
        />
        <DialogFooter>
          <SignInActions
            phase={phase}
            signedIn={signedIn}
            onClose={close}
            onRunAgain={runAgain}
            onCheckAgain={() => void recheck()}
          />
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * The footer for each step. The recovery a step calls for is the primary
 * button: running it again after a failed sign-in, checking again after a
 * failed check. Done appears only once the engine is signed in.
 */
function SignInActions({
  phase,
  signedIn,
  onClose,
  onRunAgain,
  onCheckAgain,
}: {
  phase: Phase;
  signedIn: boolean;
  onClose: () => void;
  onRunAgain: () => void;
  onCheckAgain: () => void;
}) {
  if (phase.step === "running") {
    return (
      <Button type="button" variant="outline" onClick={onClose}>
        Cancel
      </Button>
    );
  }
  if (phase.step === "checking") {
    return (
      <Button type="button" variant="outline" onClick={onClose}>
        Close
      </Button>
    );
  }
  if (phase.step === "check_failed") {
    return (
      <>
        <Button type="button" variant="outline" onClick={onClose}>
          Close
        </Button>
        <Button type="button" onClick={onCheckAgain}>
          Check again
        </Button>
      </>
    );
  }
  if (signedIn) {
    return (
      <Button type="button" onClick={onClose}>
        Done
      </Button>
    );
  }
  return (
    <>
      <Button type="button" variant="outline" onClick={onClose}>
        Close
      </Button>
      <Button type="button" onClick={onRunAgain}>
        Run it again
      </Button>
    </>
  );
}

/** One line under the terminal: what happens next, or what the check found. */
function SignInStatus({
  label,
  phase,
  signedIn,
  unconfirmed,
}: {
  label: string;
  phase: Phase;
  signedIn: boolean;
  unconfirmed: boolean;
}) {
  if (phase.step === "running") {
    return (
      <p className="text-muted-foreground text-sm">
        Tidebreak checks the sign-in again when the command finishes.
      </p>
    );
  }
  if (phase.step === "checking") {
    return (
      <p
        className="text-muted-foreground flex items-center gap-2 text-sm"
        role="status"
      >
        <Spinner className="size-3.5" aria-hidden="true" />
        Checking the sign-in…
      </p>
    );
  }
  if (phase.step === "check_failed") {
    return (
      <p
        className={cn("flex items-start gap-2 text-sm", STATUS_TEXT.critical)}
        role="alert"
      >
        <TriangleAlert
          className="mt-0.5 size-3.5 shrink-0"
          aria-hidden="true"
        />
        {phase.message}
      </p>
    );
  }
  return (
    <p
      className={cn(
        "flex items-start gap-2 text-sm",
        STATUS_TEXT[signedIn ? "ready" : "warning"],
      )}
      role="status"
    >
      {signedIn ? (
        <CircleCheck className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
      ) : (
        <TriangleAlert
          className="mt-0.5 size-3.5 shrink-0"
          aria-hidden="true"
        />
      )}
      {signedIn
        ? `${label} is signed in.`
        : unconfirmed
          ? `Tidebreak could not confirm the ${label} sign-in. You can still pick ${label}.`
          : `${label} is still signed out.`}
    </p>
  );
}

/**
 * Draws the sign-in the store asks for. Mounted once, in the app shell.
 *
 * The re-check reads the doctor report every code surface shares, then tells
 * any surface holding its own copy to read it again.
 */
export function EngineSignInHost() {
  const { client } = useApp();
  const kind = useEngineSignInStore((state) => state.kind);
  const close = useEngineSignInStore((state) => state.close);
  const recordCheck = useEngineSignInStore((state) => state.recordCheck);
  const doctor = useCodeCatalogStore((state) => state.doctor);
  const refreshDoctor = useCodeCatalogStore((state) => state.refreshDoctor);
  if (!kind) return null;
  return (
    <EngineSignInDialog
      // A different engine is a different sign-in, from the start.
      key={kind}
      client={client}
      kind={kind}
      entry={doctor?.harnesses.find((entry) => entry.kind === kind)}
      open
      onOpenChange={(next) => {
        if (!next) close();
      }}
      onRecheck={async () => {
        try {
          await refreshDoctor(client);
        } finally {
          recordCheck();
        }
      }}
    />
  );
}

/**
 * Under an engine picker: the engine picked is on disk but still needs a
 * sign-in, and the button that runs it.
 *
 * Downloading an engine does not sign it in. Without this line a reader who
 * picked a fresh engine saw Create go quiet after the download, or, where the
 * sign-in could not be confirmed, met the sign-in only when the first turn
 * failed.
 */
export function HarnessSignInNote({ entry }: { entry: HarnessDoctorEntry }) {
  if (!harnessNeedsSignIn(entry)) return null;
  const label = HARNESS_LABELS[entry.kind];
  return (
    <p className="flex items-center gap-2 text-xs" role="status">
      <TriangleAlert
        className={cn("size-3.5 shrink-0", STATUS_TEXT.warning)}
        aria-hidden="true"
      />
      <span className="text-muted-foreground min-w-0 flex-1">
        {entry.authenticated === false
          ? `${label} is downloaded but still needs a sign-in.`
          : `Tidebreak could not confirm the ${label} sign-in. You can still start it.`}
      </span>
      <EngineSignInButton entry={entry} size="xs" />
    </p>
  );
}

/**
 * A Sign in button for one engine, when its row needs one.
 *
 * Renders nothing for an engine that is signed in, has no command to run, or
 * signs in through the gateway, so every surface can place it without
 * repeating that test.
 */
export function EngineSignInButton({
  entry,
  className,
  size = "sm",
  children,
}: {
  entry: HarnessDoctorEntry;
  className?: string;
  size?: "sm" | "xs";
  children?: ReactNode;
}) {
  if (!harnessNeedsSignIn(entry)) return null;
  return (
    <Button
      type="button"
      variant="outline"
      size={size}
      className={cn("shrink-0", className)}
      onClick={() => openEngineSignIn(entry.kind)}
    >
      <LogIn className="size-3.5" aria-hidden="true" />
      {children ?? "Sign in"}
    </Button>
  );
}
