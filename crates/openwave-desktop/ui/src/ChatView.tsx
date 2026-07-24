import { useEffect, useRef, useState, type ReactNode } from "react";
import type {
  AgentRun,
  Chat,
  PendingFolderAccessRequest,
  PendingUserQuestions,
  UserQuestionAnswer,
} from "./api";
import { AgentActivityPanel } from "./AgentActivityPanel";
import { followScrollBehavior, isNearBottom, scrollToLatest } from "./ChatScroll";
import { useChatSessionStore } from "./ChatSessionStore";
import { Composer } from "./Composer";
import type { FolderAccessDecision } from "./host";
import { MessageList } from "./MessageList";
import { useTranscriptVisible } from "./TranscriptVisibility";
import { ArrowDown } from "lucide-react";

export type ChatViewProps = {
  chat: Chat;
  hydrated: boolean;
  nativeHost: boolean;
  deletingChat: boolean;
  agentRuns: AgentRun[];
  agentRunsLoading: boolean;
  agentRunsError: string | null;
  stoppingRunIds: Set<string>;
  stopErrorRunIds: Set<string>;
  onRetryAgentRuns: () => void;
  onStopSandboxRun: (runId: string) => void;
  folderAccessRequests: PendingFolderAccessRequest[];
  userQuestionRequests: PendingUserQuestions[];
  resolvingFolderCalls: Set<string>;
  folderAccessErrors: Record<string, string>;
  answeringQuestionCalls: Set<string>;
  userQuestionErrors: Record<string, string>;
  decidingApprovalCalls: Set<string>;
  approvalErrors: Record<string, string>;
  onApproval: (
    callId: string,
    decision: "approve" | "reject",
    remember?: boolean,
  ) => void;
  onFolderAccessDecision: (
    callId: string,
    decision: FolderAccessDecision,
  ) => void;
  onFolderAccessCancel: (callId: string, turnId: string) => void;
  onAnswerUserQuestions: (
    callId: string,
    answers: UserQuestionAnswer[],
  ) => void;
  onUserQuestionsCancel: (turnId: string) => void;
  draft: string;
  composerModelMenu: ReactNode;
  attachingSource: boolean;
  attachedSourceName: string | null;
  sourceAttachmentError: string | null;
  cancelError: string | null;
  cancelPendingTurnId: string | null;
  steerError: string | null;
  steerStatus: string | null;
  steerPendingTurnId: string | null;
  onDraftChange: (value: string) => void;
  onAddSource: () => Promise<void>;
  onDismissAttachedSource: () => void;
  onSelectPrompt: (prompt: string) => void;
  onSend: () => Promise<void>;
  onSteer: () => Promise<void>;
  onStop: () => Promise<void>;
};

/**
 * The chat pane: agent activity, transcript, and composer, rendered as the
 * body of the chat workspace. Reads the live session (messages, busy, active
 * turn) straight from the session store.
 * Mount with `key={chat.id}` so scroll-follow state resets per conversation.
 */
export function ChatView({
  chat,
  hydrated,
  nativeHost,
  deletingChat,
  agentRuns,
  agentRunsLoading,
  agentRunsError,
  stoppingRunIds,
  stopErrorRunIds,
  onRetryAgentRuns,
  onStopSandboxRun,
  folderAccessRequests,
  userQuestionRequests,
  resolvingFolderCalls,
  folderAccessErrors,
  answeringQuestionCalls,
  userQuestionErrors,
  decidingApprovalCalls,
  approvalErrors,
  onApproval,
  onFolderAccessDecision,
  onFolderAccessCancel,
  onAnswerUserQuestions,
  onUserQuestionsCancel,
  draft,
  composerModelMenu,
  attachingSource,
  attachedSourceName,
  sourceAttachmentError,
  cancelError,
  cancelPendingTurnId,
  steerError,
  steerStatus,
  steerPendingTurnId,
  onDraftChange,
  onAddSource,
  onDismissAttachedSource,
  onSelectPrompt,
  onSend,
  onSteer,
  onStop,
}: ChatViewProps) {
  const transcriptVisible = useTranscriptVisible();
  const messages = useChatSessionStore((session) => session.messages);
  const busy = useChatSessionStore((session) => session.busy);
  const activeTurnId = useChatSessionStore((session) => session.activeTurnId);
  const reasoningActive = useChatSessionStore(
    (session) => session.reasoningActive,
  );

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const followsLatestRef = useRef(true);
  const visibleContinuationCallIdsRef = useRef<Set<string>>(new Set());
  const [hasUnreadActivity, setHasUnreadActivity] = useState(false);

  useEffect(() => {
    const scroll = scrollRef.current;
    // Scrolling a transcript that has been expanded away does nothing, so this
    // also runs on the way back to put a following reader at the latest message.
    if (!scroll || !transcriptVisible) return;
    if (followsLatestRef.current) {
      scrollToLatest(scroll, followScrollBehavior(busy));
    } else {
      setHasUnreadActivity(true);
    }
  }, [messages, busy, transcriptVisible]);

  useEffect(() => {
    const next = new Set([
      ...folderAccessRequests.map((request) => request.callId),
      ...userQuestionRequests.map((request) => request.callId),
    ]);
    const gainedRequest = [...next].some(
      (callId) => !visibleContinuationCallIdsRef.current.has(callId),
    );
    visibleContinuationCallIdsRef.current = next;
    if (!gainedRequest) return;
    const scroll = scrollRef.current;
    if (!scroll) return;
    if (followsLatestRef.current) {
      scrollToLatest(scroll, followScrollBehavior(false));
    } else {
      setHasUnreadActivity(true);
    }
  }, [folderAccessRequests, userQuestionRequests]);

  return (
    <section className="chat-pane">
      <AgentActivityPanel
        runs={agentRuns}
        loading={agentRunsLoading}
        error={agentRunsError}
        onRetry={onRetryAgentRuns}
        stoppingRunIds={stoppingRunIds}
        stopErrorRunIds={stopErrorRunIds}
        onStop={onStopSandboxRun}
      />

      <div className="message-view">
        <MessageList
          messages={messages}
          folderAccessRequests={folderAccessRequests}
          userQuestionRequests={userQuestionRequests}
          nativeHost={nativeHost}
          nativeBusy={resolvingFolderCalls.size > 0}
          resolvingFolderCalls={resolvingFolderCalls}
          folderAccessErrors={folderAccessErrors}
          answeringQuestionCalls={answeringQuestionCalls}
          userQuestionErrors={userQuestionErrors}
          decidingApprovalCalls={decidingApprovalCalls}
          approvalErrors={approvalErrors}
          busy={busy}
          reasoningActive={reasoningActive}
          scrollRef={scrollRef}
          onScroll={(event) => {
            const followsLatest = isNearBottom(event.currentTarget);
            followsLatestRef.current = followsLatest;
            if (followsLatest) setHasUnreadActivity(false);
          }}
          onApproval={onApproval}
          onFolderAccessDecision={onFolderAccessDecision}
          onFolderAccessCancel={onFolderAccessCancel}
          onAnswerUserQuestions={onAnswerUserQuestions}
          onUserQuestionsCancel={onUserQuestionsCancel}
          onSelectPrompt={onSelectPrompt}
          hydrated={hydrated}
        />
        {hasUnreadActivity && (
          <button
            type="button"
            className="new-activity"
            onClick={() => {
              followsLatestRef.current = true;
              setHasUnreadActivity(false);
              if (scrollRef.current) {
                scrollToLatest(scrollRef.current, followScrollBehavior(false));
              }
            }}
          >
            New activity
            <ArrowDown size={13} />
          </button>
        )}
      </div>

      <Composer
        activeTurnId={activeTurnId}
        busy={busy}
        cancelError={cancelError}
        cancelPending={
          activeTurnId !== null && cancelPendingTurnId === activeTurnId
        }
        disabled={deletingChat}
        draft={draft}
        modelMenu={composerModelMenu}
        canAttachSource={nativeHost}
        attachingSource={attachingSource}
        attachedSourceName={attachedSourceName}
        sourceAttachmentError={sourceAttachmentError}
        onAddSource={onAddSource}
        onDismissAttachedSource={onDismissAttachedSource}
        onDraftChange={onDraftChange}
        onSend={onSend}
        onSteer={onSteer}
        onStop={onStop}
        resetKey={chat.id}
        steerError={steerError}
        steerPending={
          activeTurnId !== null && steerPendingTurnId === activeTurnId
        }
        steerStatus={steerStatus}
      />
    </section>
  );
}
