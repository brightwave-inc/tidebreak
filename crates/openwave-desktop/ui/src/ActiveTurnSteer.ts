export const MAX_STEER_CHARACTERS = 65_536;

/**
 * The conversation and turn a piece of guidance was aimed at.
 *
 * This used to carry the root's conversation-selection counter as a third
 * discriminator, from when one fence spanned every chat. The fence now belongs
 * to the hook that owns a single conversation's turn controls, and the counter
 * no longer distinguishes anything the chat id does not: a chat *switch* is
 * either an unmount, which takes the fence with it, or a new `chatId`, which
 * the id comparison below already rejects; a chat *deletion* leaves the doomed
 * conversation mounted under its own id, and is handled by invalidating the
 * fence and by refusing to name a chat that is on its way out as the current
 * target. See [useTurnControls].
 */
export type ActiveTurnTarget = {
  chatId: string;
  turnId: string;
};

export type ActiveTurnSteerRequest = ActiveTurnTarget & {
  content: string;
  draftSnapshot: string;
  steerId: string;
  voiceInputUsed: boolean;
  /** Skills named for this guidance alone, under the steer's own budget. */
  invokedSkills: readonly string[];
};

export function canBeginActiveTurnSteer(input: {
  busy: boolean;
  turnId: string | null;
  cancelRequestTurnId: string | null;
  deletionInFlight: boolean;
}): input is typeof input & { turnId: string } {
  return (
    input.busy &&
    input.turnId !== null &&
    input.cancelRequestTurnId === null &&
    !input.deletionInFlight
  );
}

function sameSkills(
  left: readonly string[],
  right: readonly string[],
): boolean {
  return (
    left.length === right.length &&
    left.every((skill, index) => skill === right[index])
  );
}

function sameTarget(
  left: ActiveTurnTarget,
  right: ActiveTurnTarget,
): boolean {
  return left.chatId === right.chatId && left.turnId === right.turnId;
}

export class ActiveTurnSteerFence {
  private pending: ActiveTurnSteerRequest | null = null;
  private retryable: ActiveTurnSteerRequest | null = null;

  begin(
    target: ActiveTurnTarget,
    draft: string,
    createId: () => string,
    voiceInputUsed = false,
    invokedSkills: readonly string[] = [],
  ): ActiveTurnSteerRequest | null {
    const content = draft.trim();
    if (
      this.pending ||
      !content ||
      content.includes("\0") ||
      [...content].length > MAX_STEER_CHARACTERS
    ) {
      return null;
    }

    const request =
      this.retryable &&
      sameTarget(this.retryable, target) &&
      this.retryable.content === content &&
      this.retryable.voiceInputUsed === voiceInputUsed &&
      sameSkills(this.retryable.invokedSkills, invokedSkills)
        ? { ...this.retryable, draftSnapshot: draft }
        : {
            ...target,
            content,
            draftSnapshot: draft,
            steerId: createId(),
            voiceInputUsed,
            invokedSkills: [...invokedSkills],
          };
    this.retryable = null;
    this.pending = request;
    return request;
  }

  canApplyResponse(
    request: ActiveTurnSteerRequest,
    currentTarget: ActiveTurnTarget,
  ): boolean {
    return this.pending === request && sameTarget(request, currentTarget);
  }

  finish(request: ActiveTurnSteerRequest): void {
    if (this.pending !== request) return;
    this.pending = null;
    this.retryable = null;
  }

  fail(request: ActiveTurnSteerRequest): void {
    if (this.pending !== request) return;
    this.pending = null;
    this.retryable = request;
  }

  invalidate(): void {
    this.pending = null;
    this.retryable = null;
  }
}

export function shouldClearAcceptedSteerDraft(
  request: ActiveTurnSteerRequest,
  currentDraft: string,
): boolean {
  return request.draftSnapshot === currentDraft;
}
