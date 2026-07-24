import { useEffect, useRef, useState, type ReactNode } from "react";
import type { AgentRun, ApiClient, Chat } from "./api";
import { AgentActivityPanel } from "./AgentActivityPanel";
import { followScrollBehavior, isNearBottom, scrollToLatest } from "./ChatScroll";
import { useChatSessionStore } from "./ChatSessionStore";
import { Composer } from "./Composer";
import { MessageList } from "./MessageList";
import { useTranscriptVisible } from "./TranscriptVisibility";
import { useFolderAccessRequests } from "./useFolderAccessRequests";
import { useToolApprovals } from "./useToolApprovals";
import { useUserQuestions } from "./useUserQuestions";
import { ArrowDown } from "lucide-react";

export type ChatViewProps = {
  client: ApiClient;
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
  client,
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
  const folderAccess = useFolderAccessRequests(client, chat.id);
  const userQuestions = useUserQuestions(client, chat.id);
  const approvals = useToolApprovals(client, chat.id);
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
      ...folderAccess.requests.map((request) => request.callId),
      ...userQuestions.requests.map((request) => request.callId),
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
  }, [folderAccess.requests, userQuestions.requests]);

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
          folderAccessRequests={folderAccess.requests}
          userQuestionRequests={userQuestions.requests}
          nativeHost={nativeHost}
          nativeBusy={folderAccess.resolving.size > 0}
          resolvingFolderCalls={folderAccess.resolving}
          folderAccessErrors={folderAccess.errors}
          answeringQuestionCalls={userQuestions.answering}
          userQuestionErrors={userQuestions.errors}
          decidingApprovalCalls={approvals.deciding}
          approvalErrors={approvals.errors}
          busy={busy}
          reasoningActive={reasoningActive}
          scrollRef={scrollRef}
          onScroll={(event) => {
            const followsLatest = isNearBottom(event.currentTarget);
            followsLatestRef.current = followsLatest;
            if (followsLatest) setHasUnreadActivity(false);
          }}
          onApproval={approvals.decide}
          onFolderAccessDecision={folderAccess.decide}
          onFolderAccessCancel={folderAccess.cancel}
          onAnswerUserQuestions={userQuestions.answer}
          onUserQuestionsCancel={userQuestions.cancel}
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
