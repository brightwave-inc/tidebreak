import { useEffect, useRef, useState, type ReactNode } from "react";
import type { Chat } from "./api";
import { AgentActivityPanel } from "./AgentActivityPanel";
import { followScrollBehavior, isNearBottom, scrollToLatest } from "./ChatScroll";
import { useChatSessionStore } from "./ChatSessionStore";
import { Composer } from "./Composer";
import { useConversationRequests } from "./ConversationRequests";
import { MessageList } from "./MessageList";
import { useTranscriptVisible } from "./TranscriptVisibility";
import { ArrowDown } from "lucide-react";

export type ChatViewProps = {
  chat: Chat;
  hydrated: boolean;
  nativeHost: boolean;
  deletingChat: boolean;
  draft: string;
  composerModelMenu: ReactNode;
  attachingSource: boolean;
  attachedSourceName: string | null;
  sourceAttachmentError: string | null;
  onDraftChange: (value: string) => void;
  onAddSource: () => Promise<void>;
  onDismissAttachedSource: () => void;
  onSelectPrompt: (prompt: string) => void;
  onSend: () => Promise<void>;
};

/**
 * The chat pane: agent activity, transcript, and composer, rendered as the
 * body of the chat workspace. Reads the live session (messages, busy, active
 * turn) straight from the session store, and what the conversation is waiting
 * on from its request context; the caller supplies only the composer's draft.
 * The conversation-scoped provider above it carries `key={chat.id}`, which is
 * what resets scroll-follow state per conversation.
 */
export function ChatView({
  chat,
  hydrated,
  nativeHost,
  deletingChat,
  draft,
  composerModelMenu,
  attachingSource,
  attachedSourceName,
  sourceAttachmentError,
  onDraftChange,
  onAddSource,
  onDismissAttachedSource,
  onSelectPrompt,
  onSend,
}: ChatViewProps) {
  const requests = useConversationRequests();
  const transcriptVisible = useTranscriptVisible();
  const messages = useChatSessionStore((session) => session.messages);
  const busy = useChatSessionStore((session) => session.busy);
  const activeTurnId = useChatSessionStore((session) => session.activeTurnId);
  const reasoningActive = useChatSessionStore(
    (session) => session.reasoningActive,
  );

  const { folderAccessRequests, userQuestionRequests } = requests;
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
        runs={requests.agentRuns}
        loading={requests.agentRunsLoading}
        error={requests.agentRunsError}
        onRetry={requests.refreshAgentRuns}
        stoppingRunIds={requests.stoppingRunIds}
        stopErrorRunIds={requests.stopErrorRunIds}
        onStop={requests.stopSandboxRun}
      />

      <div className="message-view">
        <MessageList
          messages={messages}
          folderAccessRequests={folderAccessRequests}
          userQuestionRequests={userQuestionRequests}
          nativeHost={nativeHost}
          nativeBusy={requests.resolvingFolderCalls.size > 0}
          resolvingFolderCalls={requests.resolvingFolderCalls}
          folderAccessErrors={requests.folderAccessErrors}
          answeringQuestionCalls={requests.answeringQuestionCalls}
          userQuestionErrors={requests.userQuestionErrors}
          decidingApprovalCalls={requests.decidingApprovalCalls}
          approvalErrors={requests.approvalErrors}
          busy={busy}
          reasoningActive={reasoningActive}
          scrollRef={scrollRef}
          onScroll={(event) => {
            const followsLatest = isNearBottom(event.currentTarget);
            followsLatestRef.current = followsLatest;
            if (followsLatest) setHasUnreadActivity(false);
          }}
          onApproval={requests.decideApproval}
          onFolderAccessDecision={requests.decideFolderAccess}
          onFolderAccessCancel={requests.cancelFolderAccess}
          onAnswerUserQuestions={requests.answerUserQuestions}
          onUserQuestionsCancel={requests.cancelUserQuestions}
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
        cancelError={requests.cancelError}
        cancelPending={
          activeTurnId !== null && requests.cancelPendingTurnId === activeTurnId
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
        onDraftChange={(value) => {
          // Typing supersedes whatever the last steer reported about a draft
          // that no longer exists.
          requests.clearSteerFeedback();
          onDraftChange(value);
        }}
        onSend={onSend}
        onSteer={requests.steerActiveTurn}
        onStop={requests.cancelActiveTurn}
        resetKey={chat.id}
        steerError={requests.steerError}
        steerPending={
          activeTurnId !== null && requests.steerPendingTurnId === activeTurnId
        }
        steerStatus={requests.steerStatus}
      />
    </section>
  );
}
