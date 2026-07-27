import {
  useEffect,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";
import type { ApiClient, Chat } from "./api";
import { followScrollBehavior, isNearBottom, scrollToLatest } from "./ChatScroll";
import { useChatSessionStore } from "./ChatSessionStore";
import { Composer, type ComposerImages } from "./Composer";
import { MessageList } from "./MessageList";
import { useTranscriptVisible } from "./TranscriptVisibility";
import { useFolderAccessRequests } from "./useFolderAccessRequests";
import { useToolApprovals } from "./useToolApprovals";
import { useTurnControls } from "./useTurnControls";
import { useUserQuestions } from "./useUserQuestions";
import { ArrowDown } from "lucide-react";

export type ChatViewProps = {
  client: ApiClient;
  chat: Chat;
  hydrated: boolean;
  nativeHost: boolean;
  deletingChat: boolean;
  draft: string;
  /** The same draft, readable synchronously — see [useTurnControls]. */
  draftRef: RefObject<string>;
  composerModelMenu: ReactNode;
  composerImages: ComposerImages;
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
 * turn) straight from the session store, and owns this conversation's pending
 * requests, agent runs, and turn controls.
 * Mount with `key={chat.id}` so scroll-follow state resets per conversation.
 */
export function ChatView({
  client,
  chat,
  hydrated,
  nativeHost,
  deletingChat,
  draft,
  draftRef,
  composerModelMenu,
  composerImages,
  attachingSource,
  attachedSourceName,
  sourceAttachmentError,
  onDraftChange,
  onAddSource,
  onDismissAttachedSource,
  onSelectPrompt,
  onSend,
}: ChatViewProps) {
  const transcriptVisible = useTranscriptVisible();
  const folderAccess = useFolderAccessRequests(client, chat.id);
  const userQuestions = useUserQuestions(client, chat.id);
  const approvals = useToolApprovals(client, chat.id);
  const turnControls = useTurnControls(client, chat.id, draftRef, () =>
    onDraftChange(""),
  );
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
      <div className="message-view">
        <MessageList
          messages={messages}
          chatId={chat.id}
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
        cancelError={turnControls.cancelError}
        cancelPending={
          activeTurnId !== null &&
          turnControls.cancelPendingTurnId === activeTurnId
        }
        disabled={deletingChat}
        draft={draft}
        modelMenu={composerModelMenu}
        images={composerImages}
        canAttachSource={nativeHost}
        attachingSource={attachingSource}
        attachedSourceName={attachedSourceName}
        sourceAttachmentError={sourceAttachmentError}
        onAddSource={onAddSource}
        onDismissAttachedSource={onDismissAttachedSource}
        // Typing retires the verdict on the last piece of guidance. Accepted
        // guidance clears the draft through the raw callback instead, so
        // "Guidance sent" survives the clearing it caused.
        onDraftChange={(value) => {
          turnControls.clearSteerFeedback();
          onDraftChange(value);
        }}
        onSend={onSend}
        onSteer={turnControls.steer}
        onStop={turnControls.cancel}
        resetKey={chat.id}
        steerError={turnControls.steerError}
        steerPending={
          activeTurnId !== null &&
          turnControls.steerPendingTurnId === activeTurnId
        }
        steerStatus={turnControls.steerStatus}
      />
    </section>
  );
}
