import type {
  PendingFolderAccessRequest,
  PendingOutputWritebackRequest,
  PendingPlanApproval,
  PendingUserQuestions,
} from "./api";
import type { ChatTurnEnding } from "./ChatSessionReducer";
import type { ChatMessage } from "./MessageList";
import {
  oneLine,
  waitingAnnouncement,
  workApprovalQuestion,
  type NeedsYouQuestion,
} from "./needsYou";

/**
 * The tool call a Work turn is parked on, newest first, as an announcement.
 * Empty when no approval card is waiting.
 *
 * Returns a string so a store selector over the transcript stays equal while
 * a token streams into an answer.
 */
export function approvalAnnouncement(messages: readonly ChatMessage[]): string {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message?.role === "approval" && !message.resolved) {
      return waitingAnnouncement(
        workApprovalQuestion(message.summary, message.preview ?? null),
      );
    }
  }
  return "";
}

/**
 * A folder or a file write the turn is parked on, as an announcement. These
 * cards ask for your approval too. Empty when neither is waiting.
 */
export function hostRequestAnnouncement(
  folderAccess: readonly PendingFolderAccessRequest[],
  writebacks: readonly PendingOutputWritebackRequest[],
): string {
  const folder = folderAccess[0];
  if (folder) {
    return waitingAnnouncement({
      kind: "approval",
      text: oneLine(folder.reason),
    });
  }
  const writeback = writebacks[0];
  if (writeback) {
    return waitingAnnouncement({
      kind: "approval",
      text:
        writeback.mode === "replace"
          ? "Replace an existing file?"
          : "Write a file to a connected folder?",
    });
  }
  return "";
}

/**
 * The question or plan that stands in for the composer while it waits on
 * you, as one question. `null` when neither is waiting.
 */
export function promptQuestion(
  questions: readonly PendingUserQuestions[],
  plans: readonly PendingPlanApproval[],
): NeedsYouQuestion | null {
  const asked = questions[0];
  if (asked) {
    return {
      kind: "question",
      text: oneLine(asked.questions[0]?.question ?? ""),
    };
  }
  const plan = plans[0];
  if (plan) return { kind: "plan", text: oneLine(plan.title) };
  return null;
}

/**
 * What the Work composer's status region says about the turn, or undefined
 * to let it say "Agent is responding" or "Ready to send".
 *
 * Waiting on you outranks everything. A turn that just ended says how it
 * ended. A failure says nothing here, because its notice in the transcript
 * is already an alert.
 */
export function composerTurnStatus(input: {
  waiting: string;
  busy: boolean;
  ending: ChatTurnEnding | null;
}): string | undefined {
  if (input.waiting) return input.waiting;
  if (input.busy) return undefined;
  if (input.ending === "finished") return "Response finished";
  if (input.ending === "stopped") return "Stopped";
  return undefined;
}
