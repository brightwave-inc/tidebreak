export const MAX_STEER_CHARACTERS = 65_536;

export type ActiveTurnTarget = {
  chatId: string;
  turnId: string;
  selection: number;
};

export type ActiveTurnSteerRequest = ActiveTurnTarget & {
  content: string;
  draftSnapshot: string;
  steerId: string;
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

function sameTarget(
  left: ActiveTurnTarget,
  right: ActiveTurnTarget,
): boolean {
  return (
    left.chatId === right.chatId &&
    left.turnId === right.turnId &&
    left.selection === right.selection
  );
}

export class ActiveTurnSteerFence {
  private pending: ActiveTurnSteerRequest | null = null;
  private retryable: ActiveTurnSteerRequest | null = null;

  begin(
    target: ActiveTurnTarget,
    draft: string,
    createId: () => string,
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
      this.retryable.content === content
        ? { ...this.retryable, draftSnapshot: draft }
        : {
            ...target,
            content,
            draftSnapshot: draft,
            steerId: createId(),
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
