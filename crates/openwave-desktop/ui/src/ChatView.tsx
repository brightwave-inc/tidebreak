import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";
import type { ApiClient, Chat, ModelInfo } from "./api";
import { followScrollBehavior, isNearBottom, scrollToLatest } from "./ChatScroll";
import { useChatSessionStore } from "./ChatSessionStore";
import { useComposerDraft } from "./ComposerDrafts";
import {
  Composer,
  type ComposerFiles,
  type ComposerFolders,
  type ComposerImages,
  type ComposerVoice,
} from "./Composer";
import type {
  ComposerNetwork,
  ComposerReasoning,
} from "./ComposerToolsMenu";
import { MessageList, type RetryableTurn } from "./MessageList";
import { useTranscriptVisible } from "./TranscriptVisibility";
import { useFolderAccessRequests } from "./useFolderAccessRequests";
import { useOutputWritebackRequests } from "./useOutputWritebackRequests";
import { useToolApprovals } from "./useToolApprovals";
import { useStreamStalled } from "./useStreamStalled";
import { useTurnControls } from "./useTurnControls";
import { usePlanApprovals } from "./usePlanApprovals";
import { useUserQuestions } from "./useUserQuestions";
import { useAgentRuns } from "./useAgentRuns";
import { ArrowDown } from "lucide-react";
import { cn } from "@/lib/utils";

export type ChatViewProps = {
  client: ApiClient;
  chat: Chat;
  failureModel?: ModelInfo | null;
  hydrated: boolean;
  nativeHost: boolean;
  deletingChat: boolean;
  /** The composer draft, readable synchronously — see [useTurnControls]. */
  draftRef: RefObject<string>;
  composerModelMenu: ReactNode;
  composerPermissionMenu: ReactNode;
  composerNetwork?: ComposerNetwork;
  composerReasoning?: ComposerReasoning;
  composerImages: ComposerImages;
  files: ComposerFiles;
  folders?: ComposerFolders;
  voice?: ComposerVoice;
  nativeDropTarget?: ReactNode;
  attachError: string | null;
  onDraftChange: (value: string) => void;
  onSelectPrompt: (prompt: string) => void;
  onSend: () => Promise<void>;
  /** Put a failed turn back on the wire, unchanged, as a new turn. */
  onRetryTurn?: (turn: RetryableTurn) => void;
  /** Open the outputs surface, offered on a completed background run's row. */
  onViewOutput?: () => void;
  /** Open one background run's panel beside the conversation. */
  onOpenAgentPanel?: (runId: string) => void;
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
  failureModel,
  hydrated,
  nativeHost,
  deletingChat,
  draftRef,
  composerModelMenu,
  composerPermissionMenu,
  composerNetwork,
  composerReasoning,
  composerImages,
  files,
  folders,
  voice,
  nativeDropTarget,
  attachError,
  onDraftChange,
  onSelectPrompt,
  onSend,
  onRetryTurn,
  onViewOutput,
  onOpenAgentPanel,
}: ChatViewProps) {
  const transcriptVisible = useTranscriptVisible();
  // Subscribed here rather than in the route above: a keystroke should
  // re-render the chat pane alone, never the panels beside it — a document
  // viewer that re-renders per keystroke is one unstable dependency away from
  // reloading its engine mid-typing.
  const draft = useComposerDraft(chat.id);
  const folderAccess = useFolderAccessRequests(client, chat.id);
  const outputWritebacks = useOutputWritebackRequests(client, chat.id);
  const userQuestions = useUserQuestions(client, chat.id);
  const planApprovals = usePlanApprovals(client, chat.id);
  const approvals = useToolApprovals(client, chat.id);
  const turnControls = useTurnControls(client, chat.id, draftRef, () =>
    onDraftChange(""),
  );
  const messages = useChatSessionStore((session) => session.messages);
  const busy = useChatSessionStore((session) => session.busy);
  const activeTurnId = useChatSessionStore((session) => session.activeTurnId);
  // Every applied stream event advances the seq cursor, so it doubles as the
  // liveness signal for the stall-aware working indicator.
  const lastSeq = useChatSessionStore((session) => session.lastSeq);
  const streamStalled = useStreamStalled(busy, lastSeq);
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
  const [fadeClass, setFadeClass] = useState<string | null>(null);
  // False until the transcript has been scrolled to its resting place once.
  // The first follow *places* the reader at the latest message, so it must not
  // animate: WKWebView occasionally fails to repaint the freshly created
  // scroll layer when a long smooth scroll runs during mount, leaving a
  // laid-out transcript blank until the next scroll input.
  const placedRef = useRef(false);
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
    setFadeClass(
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
    const placing = !placedRef.current;
    if (messages.length > 0) placedRef.current = true;
    if (followsLatestRef.current) {
      scrollToBottom(placing ? "auto" : followScrollBehavior(busy));
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
      <div className={cn("message-view", fadeClass)}>
        <MessageList
          messages={messages}
          failureModel={failureModel}
          chatId={chat.id}
          folderAccessRequests={folderAccess.requests}
          outputWritebackRequests={outputWritebacks.requests}
          userQuestionRequests={userQuestions.requests}
          planApprovalRequests={planApprovals.requests}
          nativeHost={nativeHost}
          nativeBusy={folderAccess.resolving.size > 0}
          resolvingFolderCalls={folderAccess.resolving}
          folderAccessErrors={folderAccess.errors}
          resolvingOutputWritebackCalls={outputWritebacks.resolving}
          outputWritebackErrors={outputWritebacks.errors}
          answeringQuestionCalls={userQuestions.answering}
          userQuestionErrors={userQuestions.errors}
          decidingPlanCalls={planApprovals.deciding}
          planApprovalErrors={planApprovals.errors}
          onPlanDecision={planApprovals.decide}
          onPlanCancel={planApprovals.cancel}
          decidingApprovalCalls={approvals.deciding}
          approvalErrors={approvals.errors}
          grantScope={chat.project_id ? "project" : "chat"}
          backgroundAgentRuns={agentRuns.runs}
          backgroundAgentRunsLoading={agentRuns.loading}
          backgroundAgentRunsError={agentRuns.error}
          onRetryBackgroundAgentRuns={agentRuns.refresh}
          onCancelBackgroundAgentRun={agentRuns.cancel}
          onLoadBackgroundAgentActivity={agentRuns.loadActivity}
          onViewBackgroundAgentOutput={onViewOutput}
          onOpenBackgroundAgent={onOpenAgentPanel}
          busy={busy}
          streamStalled={streamStalled}
          scrollRef={attachScrollRef}
          contentRef={attachContentRef}
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
          onSelectPrompt={onSelectPrompt}
          onRetryTurn={onRetryTurn}
          hydrated={hydrated}
          imageClient={client}
          changeClient={client}
        />
        <button
          type="button"
          className={cn(
            "absolute z-[1] left-1/2 bottom-3 -translate-x-1/2 inline-flex items-center justify-center rounded-full border border-border p-2 text-foreground bg-background shadow transition-[opacity,background-color] duration-150 ease-in-out opacity-0 pointer-events-none hover:bg-accent motion-reduce:transition-none",
            scrolledAway && "opacity-100 pointer-events-auto",
          )}
          aria-label="Scroll to latest"
          aria-hidden={!scrolledAway}
          tabIndex={scrolledAway ? 0 : -1}
          onClick={jumpToLatest}
        >
          <ArrowDown size={16} />
        </button>
      </div>

      <div className="px-[clamp(0.5rem,4%,5rem)] pb-2">
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
          permissionMenu={composerPermissionMenu}
          network={composerNetwork}
          reasoning={composerReasoning}
          images={composerImages}
          files={files}
          folders={folders}
          voice={voice}
          nativeDropTarget={nativeDropTarget}
          attachError={attachError}
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
      </div>
    </section>
  );
}
