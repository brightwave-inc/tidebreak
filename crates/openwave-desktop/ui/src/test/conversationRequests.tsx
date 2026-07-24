import { vi } from "vitest";
import type { ReactNode } from "react";
import {
  ConversationRequestsScope,
  type ConversationRequests,
} from "../ConversationRequests";

/** A conversation with nothing in flight, for surfaces under test. */
export function idleConversationRequests(
  overrides: Partial<ConversationRequests> = {},
): ConversationRequests {
  return {
    agentRuns: [],
    agentRunsLoading: false,
    agentRunsError: null,
    stoppingRunIds: new Set(),
    stopErrorRunIds: new Set(),
    refreshAgentRuns: vi.fn(),
    stopSandboxRun: vi.fn(),

    folderAccessRequests: [],
    resolvingFolderCalls: new Set(),
    folderAccessErrors: {},
    decideFolderAccess: vi.fn(),
    cancelFolderAccess: vi.fn(),

    userQuestionRequests: [],
    answeringQuestionCalls: new Set(),
    userQuestionErrors: {},
    answerUserQuestions: vi.fn(),
    cancelUserQuestions: vi.fn(),

    decidingApprovalCalls: new Set(),
    approvalErrors: {},
    decideApproval: vi.fn(),

    cancelPendingTurnId: null,
    cancelError: null,
    cancelActiveTurn: vi.fn(async () => {}),

    steerPendingTurnId: null,
    steerError: null,
    steerStatus: null,
    steerActiveTurn: vi.fn(async () => {}),
    clearSteerFeedback: vi.fn(),
    ...overrides,
  };
}

export function withConversationRequests(
  children: ReactNode,
  requests: ConversationRequests = idleConversationRequests(),
) {
  return (
    <ConversationRequestsScope value={requests}>
      {children}
    </ConversationRequestsScope>
  );
}
