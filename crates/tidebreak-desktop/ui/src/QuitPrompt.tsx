import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";

import type {
  QuitAgentCount,
  QuitChoice,
  QuitPromptState,
} from "./desktopLifecycle";

type QuitPromptProps = {
  prompt: QuitPromptState;
  /** Why waiting for a safe point failed, when it did. */
  error?: string | null;
  /** An answer is on its way to the shell. */
  answering?: boolean;
  onChoose: (choice: QuitChoice) => void;
  /**
   * Open the inbox, where the approvals, questions, and plans agents wait on
   * are answered.
   */
  onOpenInbox?: () => void;
};

type QuitPromptCopy = {
  title: string;
  description: string;
  stopLabel: string;
  safePointLabel: string;
  /** What to say about agents waiting for the person's answer, if any. */
  waitingNote: string | null;
};

/**
 * The sentence about agents parked on the person's answer, which the shell's
 * native fallback says too. Such an agent reaches a safe point only after the
 * person answers.
 */
function waitingForYouNote({
  agents,
  waitingForYou,
}: QuitAgentCount): string | null {
  if (waitingForYou === 0) return null;
  if (agents === 1) {
    return "It needs your answer before it can reach a safe point.";
  }
  if (waitingForYou === 1) {
    return "One of them needs your answer before it can reach a safe point.";
  }
  if (waitingForYou >= agents) {
    return "All of them need your answers before they can reach a safe point.";
  }
  return `${waitingForYou} of them need your answers before they can reach a safe point.`;
}

/** The shorter form of the same sentence, for the bar a wait shows. */
function waitingForYouShort({
  agents,
  waitingForYou,
}: QuitAgentCount): string | null {
  if (waitingForYou === 0) return null;
  if (agents === 1) return "It needs your answer first.";
  if (waitingForYou === 1) return "One needs your answer first.";
  if (waitingForYou >= agents) return "All need your answers first.";
  return `${waitingForYou} need your answers first.`;
}

/** The words of the prompt, which follow how many agents are working. */
export function quitPromptCopy(prompt: QuitPromptState): QuitPromptCopy {
  const one = "agents" in prompt && prompt.agents === 1;
  const stopLabel = one ? "Quit and stop it" : "Quit and stop them";
  const safePointLabel = one
    ? "Quit when it reaches a safe point"
    : "Quit when they reach a safe point";
  switch (prompt.phase) {
    case "asking":
      return {
        title: one
          ? "Quit while an agent is working?"
          : `Quit while ${prompt.agents} agents are working?`,
        description: one
          ? "Stopping it ends its current turn. At a safe point, a code turn finishes first, and a chat continues the next time Tidebreak opens."
          : "Stopping them ends their current turns. At a safe point, code turns finish first, and chats continue the next time Tidebreak opens.",
        stopLabel,
        safePointLabel,
        waitingNote: waitingForYouNote(prompt),
      };
    case "waiting":
      return {
        title: one
          ? "Quitting when the agent reaches a safe point"
          : `Quitting when ${prompt.agents} agents reach a safe point`,
        description:
          waitingForYouShort(prompt) ??
          "New messages wait until Tidebreak opens again.",
        stopLabel,
        safePointLabel,
        waitingNote: waitingForYouShort(prompt),
      };
    case "stopping":
      return {
        title: "Stopping agents",
        description: "Tidebreak quits as soon as they stop.",
        stopLabel,
        safePointLabel,
        waitingNote: null,
      };
    case "idle":
      return {
        title: "",
        description: "",
        stopLabel,
        safePointLabel,
        waitingNote: null,
      };
  }
}

/**
 * Asks what to do about working agents before the app quits.
 *
 * Cancel is the safe default and takes focus. Stopping the agents is the
 * destructive choice, so it wears the destructive variant. An agent waiting
 * for the person's answer is named, with a way to the inbox.
 *
 * A quit that waits for a safe point shows a bar instead of the dialog, so
 * the app stays usable: an agent parked on an approval gets there only once
 * the person answers it.
 */
export function QuitPrompt({
  prompt,
  error = null,
  answering = false,
  onChoose,
  onOpenInbox,
}: QuitPromptProps) {
  if (prompt.phase === "waiting") {
    return (
      <QuitWaitingBar
        prompt={prompt}
        answering={answering}
        onChoose={onChoose}
        onOpenInbox={onOpenInbox}
      />
    );
  }
  const copy = quitPromptCopy(prompt);
  const asking = prompt.phase === "asking";

  return (
    <AlertDialog open={prompt.phase !== "idle"}>
      {prompt.phase !== "idle" && (
        <AlertDialogContent
          className="sm:max-w-xl"
          onEscapeKeyDown={(event) => {
            event.preventDefault();
            if (asking && !answering) onChoose("cancel");
          }}
        >
          <AlertDialogHeader>
            <AlertDialogTitle className="flex items-center justify-center gap-2 sm:justify-start">
              {!asking && <Spinner aria-hidden="true" />}
              {copy.title}
            </AlertDialogTitle>
            <AlertDialogDescription>{copy.description}</AlertDialogDescription>
            {copy.waitingNote && (
              <p className="text-sm text-muted-foreground">
                {copy.waitingNote}
                {onOpenInbox && (
                  <>
                    {" "}
                    <Button
                      type="button"
                      variant="link"
                      // The 2xs size sets a height the merge can replace.
                      size="2xs"
                      className="inline h-auto border-0 p-0 align-baseline text-sm"
                      disabled={answering}
                      onClick={() => {
                        // The inbox is behind this dialog, so going there
                        // calls the quit off. Quit again after answering.
                        onChoose("cancel");
                        onOpenInbox();
                      }}
                    >
                      Open inbox
                    </Button>
                  </>
                )}
              </p>
            )}
            {error && asking && (
              <p className="text-sm text-critical" role="alert">
                Tidebreak could not reach a safe point. {error}
              </p>
            )}
          </AlertDialogHeader>
          {asking && (
            <AlertDialogFooter className="gap-2 sm:space-x-0">
              <AlertDialogCancel
                disabled={answering}
                onClick={(event) => {
                  event.preventDefault();
                  onChoose("cancel");
                }}
              >
                Cancel
              </AlertDialogCancel>
              <Button
                type="button"
                disabled={answering}
                onClick={() => onChoose("safe_point")}
              >
                {copy.safePointLabel}
              </Button>
              <Button
                type="button"
                variant="destructive"
                disabled={answering}
                onClick={() => onChoose("stop")}
              >
                {copy.stopLabel}
              </Button>
            </AlertDialogFooter>
          )}
        </AlertDialogContent>
      )}
    </AlertDialog>
  );
}

/**
 * The quit waiting for a safe point: a bar at the foot of the window that
 * counts the agents down and leaves the app usable behind it.
 */
function QuitWaitingBar({
  prompt,
  answering,
  onChoose,
  onOpenInbox,
}: {
  prompt: Extract<QuitPromptState, { phase: "waiting" }>;
  answering: boolean;
  onChoose: (choice: QuitChoice) => void;
  onOpenInbox?: () => void;
}) {
  const copy = quitPromptCopy(prompt);
  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-5 z-50 flex justify-center px-4">
      <section
        aria-label="Quitting"
        className="pointer-events-auto flex w-full min-w-0 max-w-xl flex-wrap items-center gap-x-3 gap-y-2 rounded-xl border bg-popover px-3 py-2.5 text-popover-foreground shadow-lg"
      >
        <Spinner aria-hidden="true" className="size-4 shrink-0" />
        <div className="min-w-0 flex-1 basis-64" role="status">
          <p className="text-xs font-semibold">{copy.title}</p>
          <p className="text-2xs text-muted-foreground">
            {copy.description}
            {copy.waitingNote && onOpenInbox && (
              <>
                {" "}
                <Button
                  type="button"
                  variant="link"
                  size="2xs"
                  className="inline h-auto border-0 p-0 align-baseline text-2xs"
                  onClick={onOpenInbox}
                >
                  Open inbox
                </Button>
              </>
            )}
          </p>
        </div>
        <div className="ml-auto flex shrink-0 items-center gap-2">
          <Button
            type="button"
            size="xs"
            variant="outline"
            disabled={answering}
            onClick={() => onChoose("cancel")}
          >
            Cancel
          </Button>
          <Button
            type="button"
            size="xs"
            variant="destructive"
            disabled={answering}
            onClick={() => onChoose("stop")}
          >
            {copy.stopLabel}
          </Button>
        </div>
      </section>
    </div>
  );
}
