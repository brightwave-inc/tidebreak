import {
  useCallback,
  useEffect,
  useMemo,
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
import { useOutputWritebackRequests } from "./useOutputWritebackRequests";
import { useToolApprovals } from "./useToolApprovals";
import { useTurnControls } from "./useTurnControls";
import { useUserQuestions } from "./useUserQuestions";
import { useAgentRuns } from "./useAgentRuns";
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
  attaching: boolean;
  attachedSourceName: string | null;
  attachError: string | null;
  onDraftChange: (value: string) => void;
  onAttach: () => Promise<void>;
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
  attaching,
  attachedSourceName,
  attachError,
  onDraftChange,
  onAttach,
  onDismissAttachedSource,
  onSelectPrompt,
  onSend,
}: ChatViewProps) {
  const transcriptVisible = useTranscriptVisible();
  const folderAccess = useFolderAccessRequests(client, chat.id);
  const outputWritebacks = useOutputWritebackRequests(client, chat.id);
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
  const backgroundAgentSpawnKeys = useMemo(
    () =>
      messages.flatMap((message) =>
        message.role === "tool" && message.name === "spawn_sandbox_agent"
          && message.status !== "failed"
          && message.status !== "cancelled"
          ? [message.backgroundAgentRunId ?? message.callId]
          : [],
      ),
    [messages],
  );
  const agentRuns = useAgentRuns(client, chat.id, backgroundAgentSpawnKeys);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const followsLatestRef = useRef(true);
  const isProgrammaticRef = useRef(false);
  const visibleContinuationCallIdsRef = useRef<Set<string>>(new Set());
  const scrollObserverRef = useRef<ResizeObserver | null>(null);
  const contentObserverRef = useRef<ResizeObserver | null>(null);
  const [scrolledAway, setScrolledAway] = useState(false);
  const [maskClass, setMaskClass] = useState<string | null>(null);
  // True once the reader has sent in this mounted session. Gates the turn pin so
  // a freshly loaded history reads normally, then the just-sent turn is held tall
  // enough to land near the top of the viewport.
  const [pinLastTurn, setPinLastTurn] = useState(false);

  // Reflect where the scroll sits onto the edge-fade masks: fade the top once
  // there is content above, the bottom while there is content below.
  const updateEdges = useCallback(() => {
    const scroll = scrollRef.current;
    if (!scroll) return;
    const fromTop = scroll.scrollTop > 0;
    const fromBottom =
      scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight > 1;
    setMaskClass(
      fromTop && fromBottom
        ? "is-faded-both"
        : fromTop
          ? "is-faded-top"
          : fromBottom
            ? "is-faded-bottom"
            : null,
    );
  }, []);

  // Jump to the latest message. Marked programmatic so the resulting scroll
  // events don't read as the reader deliberately scrolling away.
  const scrollToBottom = useCallback((behavior: ScrollBehavior) => {
    const scroll = scrollRef.current;
    if (!scroll) return;
    isProgrammaticRef.current = true;
    scrollToLatest(scroll, behavior);
    if (behavior === "smooth") {
      let timeout: ReturnType<typeof setTimeout>;
      const done = () => {
        clearTimeout(timeout);
        isProgrammaticRef.current = false;
      };
      scroll.addEventListener("scrollend", done, { once: true });
      timeout = setTimeout(() => {
        scroll.removeEventListener("scrollend", done);
        isProgrammaticRef.current = false;
      }, 800);
    } else {
      requestAnimationFrame(() => {
        isProgrammaticRef.current = false;
      });
    }
  }, []);

  const handleScroll = useCallback(() => {
    updateEdges();
    if (isProgrammaticRef.current) return;
    const scroll = scrollRef.current;
    if (!scroll) return;
    const away = !isNearBottom(scroll);
    setScrolledAway(away);
    // Drifting away disarms follow; re-arming is deliberate (the button, or a
    // send), never a side effect of scrolling back toward the bottom.
    if (away && followsLatestRef.current) followsLatestRef.current = false;
  }, [updateEdges]);

  // Track the scroll viewport height in a CSS variable so a pinned turn can
  // reserve roughly a screenful, and keep the edge fades honest across resizes.
  const attachScrollRef = useCallback(
    (element: HTMLDivElement | null) => {
      scrollObserverRef.current?.disconnect();
      scrollRef.current = element;
      if (!element) return;
      const observer = new ResizeObserver(() => {
        element.style.setProperty(
          "--transcript-viewport",
          `${element.clientHeight}px`,
        );
        updateEdges();
      });
      observer.observe(element);
      scrollObserverRef.current = observer;
    },
    [updateEdges],
  );

  // Follow asynchronous layout growth (image loads, markdown reflow) that no
  // React state change announces, so a following reader stays pinned to the end.
  const attachContentRef = useCallback(
    (element: HTMLDivElement | null) => {
      contentObserverRef.current?.disconnect();
      if (!element) return;
      const observer = new ResizeObserver(() => {
        if (!transcriptVisible) return;
        if (followsLatestRef.current) scrollToBottom("auto");
        const scroll = scrollRef.current;
        if (scroll) setScrolledAway(!isNearBottom(scroll));
        updateEdges();
      });
      observer.observe(element);
      contentObserverRef.current = observer;
    },
    [transcriptVisible, scrollToBottom, updateEdges],
  );

  useEffect(() => {
    // Scrolling a transcript that has been expanded away does nothing, so this
    // also runs on the way back to put a following reader at the latest message.
    if (!transcriptVisible) return;
    if (followsLatestRef.current) {
      scrollToBottom(followScrollBehavior(busy));
    }
    updateEdges();
  }, [messages, busy, transcriptVisible, scrollToBottom, updateEdges]);

  useEffect(() => {
    const next = new Set([
      ...folderAccess.requests.map((request) => request.callId),
      ...outputWritebacks.requests.map((request) => request.callId),
      ...userQuestions.requests.map((request) => request.callId),
    ]);
    const gainedRequest = [...next].some(
      (callId) => !visibleContinuationCallIdsRef.current.has(callId),
    );
    visibleContinuationCallIdsRef.current = next;
    if (!gainedRequest) return;
    if (followsLatestRef.current) scrollToBottom(followScrollBehavior(false));
  }, [
    folderAccess.requests,
    outputWritebacks.requests,
    userQuestions.requests,
    scrollToBottom,
  ]);

  const jumpToLatest = useCallback(() => {
    followsLatestRef.current = true;
    setScrolledAway(false);
    scrollToBottom(followScrollBehavior(false));
  }, [scrollToBottom]);

  const handleSend = useCallback(async () => {
    followsLatestRef.current = true;
    setScrolledAway(false);
    setPinLastTurn(true);
    await onSend();
    scrollToBottom(followScrollBehavior(false));
  }, [onSend, scrollToBottom]);

  return (
    <section className="chat-pane">
      <div className="message-view">
        <MessageList
          messages={messages}
          chatId={chat.id}
          folderAccessRequests={folderAccess.requests}
          outputWritebackRequests={outputWritebacks.requests}
          userQuestionRequests={userQuestions.requests}
          nativeHost={nativeHost}
          nativeBusy={folderAccess.resolving.size > 0}
          resolvingFolderCalls={folderAccess.resolving}
          folderAccessErrors={folderAccess.errors}
          resolvingOutputWritebackCalls={outputWritebacks.resolving}
          outputWritebackErrors={outputWritebacks.errors}
          answeringQuestionCalls={userQuestions.answering}
          userQuestionErrors={userQuestions.errors}
          decidingApprovalCalls={approvals.deciding}
          approvalErrors={approvals.errors}
          backgroundAgentRuns={agentRuns.runs}
          backgroundAgentRunsLoading={agentRuns.loading}
          backgroundAgentRunsError={agentRuns.error}
          onRetryBackgroundAgentRuns={agentRuns.refresh}
          busy={busy}
          reasoningActive={reasoningActive}
          scrollRef={attachScrollRef}
          contentRef={attachContentRef}
          maskClass={maskClass}
          pinLastTurn={pinLastTurn}
          onScroll={handleScroll}
          onApproval={approvals.decide}
          onFolderAccessDecision={folderAccess.decide}
          onFolderAccessCancel={folderAccess.cancel}
          onOutputWritebackDecision={outputWritebacks.decide}
          onOutputWritebackCancel={(callId, turnId) =>
            outputWritebacks.cancel(callId, turnId)
          }
          onAnswerUserQuestions={userQuestions.answer}
          onUserQuestionsCancel={userQuestions.cancel}
          onSelectPrompt={onSelectPrompt}
          hydrated={hydrated}
          imageClient={client}
        />
        <button
          type="button"
          className={`scroll-to-latest${scrolledAway ? " is-visible" : ""}`}
          aria-label="Scroll to latest"
          aria-hidden={!scrolledAway}
          tabIndex={scrolledAway ? 0 : -1}
          onClick={jumpToLatest}
        >
          <ArrowDown size={16} />
        </button>
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
        canAttach={nativeHost}
        attaching={attaching}
        attachedSourceName={attachedSourceName}
        attachError={attachError}
        onAttach={onAttach}
        onDismissAttachedSource={onDismissAttachedSource}
        // Typing retires the verdict on the last piece of guidance. Accepted
        // guidance clears the draft through the raw callback instead, so
        // "Guidance sent" survives the clearing it caused.
        onDraftChange={(value) => {
          turnControls.clearSteerFeedback();
          onDraftChange(value);
        }}
        onSend={handleSend}
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
