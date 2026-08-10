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
import {
  followScrollBehavior,
  isNearBottom,
  scrollToLatest,
} from "./ChatScroll";
import { useChatSessionStore } from "./ChatSessionStore";
import {
  useComposerAttachments,
  useComposerDraft,
  useComposerDrafts,
} from "./ComposerDrafts";
import {
  Composer,
  type ComposerFiles,
  type ComposerFolders,
  type ComposerImages,
  type ComposerVoice,
} from "./Composer";
import type { SlashCommandName } from "./ComposerCommands";
import { ComposerPrompt } from "./ComposerPrompt";
import { ChatUsageDialog } from "./ChatUsageDialog";
import type { ComposerNetwork, ComposerReasoning } from "./ComposerToolsMenu";
import { MessageList, type RetryableTurn } from "./MessageList";
import { revealPendingCall } from "./TranscriptFocus";
import { useTranscriptVisible } from "./TranscriptVisibility";
import { useFolderAccessRequests } from "./useFolderAccessRequests";
import { useOutputWritebackRequests } from "./useOutputWritebackRequests";
import { useToolApprovals } from "./useToolApprovals";
import { useStreamStalled } from "./useStreamStalled";
import { QueueTray } from "./QueueTray";
import { useTurnControls } from "./useTurnControls";
import { usePlanApprovals } from "./usePlanApprovals";
import { useTaskPlan } from "./useTaskPlan";
import { TaskPlanCard } from "./TaskPlanCard";
import { useUserQuestions } from "./useUserQuestions";
import { useComposerPlugins } from "./plugins/useComposerPlugins";
import { recentChatFiles } from "./ComposerMentions";
import {
  backgroundAgentSpawnKeys as spawnKeysOf,
  useAgentRuns,
} from "./useAgentRuns";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { ArrowDown } from "lucide-react";
import { toast } from "sonner";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import {
  TranscriptNavigation,
  transcriptNavigationEntries,
} from "./TranscriptNavigation";

export type ChatViewProps = {
  client: ApiClient;
  chat: Chat;
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
  /** Whether voice transcription contributed to the current draft. */
  voiceInputUsed: boolean;
  /** Retire voice origin only after the current draft is durably accepted. */
  onVoiceInputAccepted: () => void;
  nativeDropTarget?: ReactNode;
  attachError: string | null;
  onDraftChange: (value: string) => void;
  onSelectPrompt: (prompt: string) => void;
  onSend: () => Promise<void>;
  /** Queue the draft to run after the active turn; absent disables queueing. */
  onQueue?: () => Promise<void>;
  /** Put a failed turn back on the wire, unchanged, as a new turn. */
  onRetryTurn?: (turn: RetryableTurn) => void;
  /** Open one background run's panel beside the conversation. */
  onOpenAgentPanel?: (runId: string) => void;
  onOpenOutput?: (outputId: string) => void;
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
  draftRef,
  composerModelMenu,
  composerPermissionMenu,
  composerNetwork,
  composerReasoning,
  composerImages,
  files,
  folders,
  voice,
  voiceInputUsed,
  onVoiceInputAccepted,
  nativeDropTarget,
  attachError,
  onDraftChange,
  onSelectPrompt,
  onSend,
  onQueue,
  onRetryTurn,
  onOpenAgentPanel,
  onOpenOutput,
}: ChatViewProps) {
  const transcriptVisible = useTranscriptVisible();
  // Subscribed here rather than in the route above: a keystroke should
  // re-render the chat pane alone, never the panels beside it — a document
  // viewer that re-renders per keystroke is one unstable dependency away from
  // reloading its engine mid-typing.
  const draft = useComposerDraft(chat.id);
  const composerPlugins = useComposerPlugins(client);
  const invokedSkills = useComposerAttachments(chat.id).skills;
  const folderAccess = useFolderAccessRequests(client, chat.id);
  const outputWritebacks = useOutputWritebackRequests(client, chat.id);
  const userQuestions = useUserQuestions(client, chat.id);
  const planApprovals = usePlanApprovals(client, chat.id);
  const approvals = useToolApprovals(client, chat.id);
  // A question or a proposed plan is the one thing the turn wants back, so its
  // card stands in the composer's slot until it is answered.
  const pendingPromptCount =
    userQuestions.requests.length + planApprovals.requests.length;
  const turnControls = useTurnControls(
    client,
    chat.id,
    draftRef,
    () => {
      onDraftChange("");
      // Pills go with the text they were attached to: accepted guidance has
      // already carried them, and leaving them behind would silently re-invoke
      // the same skills on whatever is typed next.
      useComposerDrafts.getState().setSkills(chat.id, []);
      onVoiceInputAccepted();
    },
    voiceInputUsed,
    invokedSkills,
  );
  const messages = useChatSessionStore((session) => session.messages);
  const busy = useChatSessionStore((session) => session.busy);
  const animateStreaming = useChatSessionStore(
    (session) => session.animateStreaming,
  );
  const compacting = useChatSessionStore((session) => session.compacting);
  const activeTurnId = useChatSessionStore((session) => session.activeTurnId);
  // Every applied stream event advances the seq cursor, so it doubles as the
  // liveness signal for the stall-aware working indicator.
  const lastSeq = useChatSessionStore((session) => session.lastSeq);
  const streamStalled = useStreamStalled(busy, lastSeq);
  const backgroundAgentSpawnKeys = useMemo(
    () => spawnKeysOf(messages),
    [messages],
  );
  const agentRuns = useAgentRuns(client, chat.id, backgroundAgentSpawnKeys);
  const taskPlan = useTaskPlan(client, chat.id);
  // The plan belongs to the turn that wrote it, so liveness comes from the
  // session the transcript already tracks rather than from another read: a
  // plan whose turn is not the running one describes work that has stopped.
  const taskPlanLive =
    taskPlan !== null && busy && activeTurnId === taskPlan.turn_id;
  // The files already on this conversation, so `@` can name one instead of
  // sending the reader back to the picker for a document we are already
  // holding. Read from the transcript rather than fetched: these are the
  // attachments of the messages on screen.
  const composerFiles = useMemo(
    () => ({
      ...files,
      recent: recentChatFiles(
        messages.flatMap((message) =>
          message.role === "user" ? [message] : [],
        ),
        files.items,
      ),
    }),
    [files, messages],
  );
  const composerHistory = useMemo(
    () =>
      messages.flatMap((message) =>
        message.role === "user" && message.text.trim() ? [message.text] : [],
      ).reverse(),
    [messages],
  );

  // Built-in `/` commands run here rather than being sent, so each one owns
  // whatever local state it needs — a dialog, in `/usage`'s case.
  const [usageOpen, setUsageOpen] = useState(false);
  const [compactRequested, setCompactRequested] = useState(false);
  /**
   * Summarize what is behind this conversation, on request.
   *
   * The success path is already told: the server journals the same compaction
   * events a turn's own pass emits, and the transcript grows its divider. What
   * needs saying here is the two outcomes the journal is silent about — a chat
   * with nothing worth summarizing yet, and a request that failed.
   */
  const runCompaction = useCallback(
    async (focus: string) => {
      if (busy) {
        toast.error("Wait for the current response to finish before compacting.");
        return;
      }
      if (compactRequested) return;
      setCompactRequested(true);
      try {
        const run = await client.compactChat(chat.id, focus || undefined);
        if (run.compacted) {
          toast.success("Summarized the earlier part of this conversation");
        } else {
          toast.message("Nothing to compact yet — this conversation still fits.");
        }
      } catch (caught) {
        toast.error(
          friendlyErrorMessage(caught, "Could not compact this conversation."),
        );
      } finally {
        setCompactRequested(false);
      }
    },
    [busy, chat.id, client, compactRequested],
  );
  const runSlashCommand = useCallback(
    (name: SlashCommandName, argument: string) => {
      switch (name) {
        case "usage":
          setUsageOpen(true);
          return;
        case "compact":
          void runCompaction(argument);
          return;
      }
    },
    [runCompaction],
  );

  const navigate = useNavigate();
  const search = useSearch({ strict: false }) as {
    focus?: string;
    at?: string;
  };
  const focusCallId = search.focus;
  const anchoredMessageId = search.at;
  const navigationSignature = messages
    .flatMap((message) => {
      if (message.role === "user") return [message.id, message.text];
      if (message.role === "tool") {
        return [message.id, message.name, message.status];
      }
      return [];
    })
    .join("\0");
  const navigationEntries = useMemo(
    () => transcriptNavigationEntries(messages),
    // Assistant streaming replaces the messages array every token, but does
    // not change the table of contents. Keep the rail's observers mounted until
    // one of the user/tool fields it actually presents changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [navigationSignature],
  );

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [scrollElement, setScrollElement] = useState<HTMLDivElement | null>(null);
  /** The composer's slot, which a pending question or plan card takes over. */
  const promptSlotRef = useRef<HTMLDivElement | null>(null);
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
      setScrollElement(element);
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

  // A deep link from the inbox names the parked call it was opened for. The
  // card it decides is mounted from a separate poll, so the reveal is retried
  // for a short while rather than given up on at first paint, and the search
  // param is dropped once honored so a reload does not re-scroll a transcript
  // the reader has since moved through.
  useEffect(() => {
    if (!focusCallId || !transcriptVisible) return;
    let settled = false;
    const clear = () => {
      settled = true;
      window.clearInterval(timer);
      window.clearTimeout(deadline);
      void navigate({
        to: "/c/$chatId",
        params: { chatId: chat.id },
        search: (previous: Record<string, unknown>) => ({
          ...previous,
          focus: undefined,
        }),
        replace: true,
      });
    };
    const attempt = () => {
      if (settled) return;
      if (revealPendingCall(scrollRef.current, focusCallId)) {
        // Arriving at a specific card means the reader is no longer following
        // the end of the transcript; letting follow stay armed would scroll them
        // straight back off it.
        followsLatestRef.current = false;
        setScrolledAway(true);
        clear();
        return;
      }
      // A question or a proposed plan stands in the composer's slot rather than
      // the transcript, so it is on screen already: pointing it out is the whole
      // reveal, and the transcript is left following its end.
      if (revealPendingCall(promptSlotRef.current, focusCallId)) clear();
    };
    const timer = window.setInterval(attempt, 120);
    const deadline = window.setTimeout(clear, 5_000);
    attempt();
    return () => {
      window.clearInterval(timer);
      window.clearTimeout(deadline);
    };
  }, [focusCallId, transcriptVisible, chat.id, navigate]);

  // The router's hash is already occupied by hash history, so a rail jump is
  // represented by `?at=`. Keeping it in the URL makes an anchored reload land
  // in the same place; it is cleared only when the reader returns to the tail.
  useEffect(() => {
    if (!anchoredMessageId || !transcriptVisible || !scrollElement) return;
    const frame = window.requestAnimationFrame(() => {
      const target = Array.from(
        scrollElement.querySelectorAll<HTMLElement>(
          "[data-transcript-anchor]",
        ),
      ).find(
        (element) => element.dataset.transcriptAnchor === anchoredMessageId,
      );
      if (!target) return;
      followsLatestRef.current = false;
      setScrolledAway(true);
      isProgrammaticRef.current = true;
      const containerRect = scrollElement.getBoundingClientRect();
      const targetRect = target.getBoundingClientRect();
      scrollElement.scrollTo({
        top: Math.max(
          0,
          scrollElement.scrollTop + targetRect.top - containerRect.top - 24,
        ),
        behavior: "smooth",
      });
      window.setTimeout(() => {
        isProgrammaticRef.current = false;
      }, 800);
    });
    return () => window.cancelAnimationFrame(frame);
  }, [anchoredMessageId, messages, scrollElement, transcriptVisible]);

  const jumpToMessage = useCallback(
    (anchorId: string) => {
      void navigate({
        to: "/c/$chatId",
        params: { chatId: chat.id },
        search: (previous: Record<string, unknown>) => ({
          ...previous,
          at: anchorId,
        }),
        replace: true,
      });
    },
    [chat.id, navigate],
  );

  const jumpToLatest = useCallback(() => {
    followsLatestRef.current = true;
    setScrolledAway(false);
    if (anchoredMessageId) {
      void navigate({
        to: "/c/$chatId",
        params: { chatId: chat.id },
        search: (previous: Record<string, unknown>) => ({
          ...previous,
          at: undefined,
        }),
        replace: true,
      });
    }
    scrollToBottom(followScrollBehavior(false));
  }, [anchoredMessageId, chat.id, navigate, scrollToBottom]);

  const handleSend = useCallback(async () => {
    followsLatestRef.current = true;
    setScrolledAway(false);
    setPinLastTurn(true);
    if (anchoredMessageId) {
      await navigate({
        to: "/c/$chatId",
        params: { chatId: chat.id },
        search: (previous: Record<string, unknown>) => ({
          ...previous,
          at: undefined,
        }),
        replace: true,
      });
    }
    await onSend();
    scrollToBottom(followScrollBehavior(false));
  }, [anchoredMessageId, chat.id, navigate, onSend, scrollToBottom]);

  return (
    <section className="chat-pane">
      {/* Mounted only while it is up: the dialog reads the model catalog and
          the chat's finished turns, and neither is worth holding open behind a
          conversation nobody has asked about. */}
      {usageOpen && (
        <ChatUsageDialog
          client={client}
          chat={chat}
          open={usageOpen}
          onOpenChange={setUsageOpen}
        />
      )}
      <div className={cn("message-view", fadeClass)}>
        <MessageList
          messages={messages}
          chatId={chat.id}
          folderAccessRequests={folderAccess.requests}
          outputWritebackRequests={outputWritebacks.requests}
          pendingPromptCount={pendingPromptCount}
          nativeHost={nativeHost}
          nativeBusy={folderAccess.resolving.size > 0}
          resolvingFolderCalls={folderAccess.resolving}
          folderAccessErrors={folderAccess.errors}
          resolvingOutputWritebackCalls={outputWritebacks.resolving}
          outputWritebackErrors={outputWritebacks.errors}
          decidingApprovalCalls={approvals.deciding}
          approvalErrors={approvals.errors}
          grantScope={chat.project_id ? "project" : "chat"}
          backgroundAgentRuns={agentRuns.runs}
          backgroundAgentRunsLoading={agentRuns.loading}
          backgroundAgentRunsError={agentRuns.error}
          onRetryBackgroundAgentRuns={agentRuns.refresh}
          onCancelBackgroundAgentRun={agentRuns.cancel}
          onLoadBackgroundAgentActivity={agentRuns.loadActivity}
          onLoadBackgroundAgentTaskPlan={agentRuns.loadTaskPlan}
          onLoadBackgroundAgentProgress={agentRuns.loadProgress}
          onOpenBackgroundAgent={onOpenAgentPanel}
          onOpenOutput={onOpenOutput}
          backgroundAgentClient={client}
          busy={busy}
          animateStreaming={animateStreaming}
          compacting={compacting}
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
          onSelectPrompt={onSelectPrompt}
          onRetryTurn={onRetryTurn}
          hydrated={hydrated}
          imageClient={client}
          executionConfigClient={client}
          changeClient={client}
        />
        <TranscriptNavigation
          entries={navigationEntries}
          scrollElement={scrollElement}
          activeAnchor={anchoredMessageId}
          onJump={jumpToMessage}
        />
        <button
          type="button"
          className={cn(
            "absolute z-[1] left-1/2 bottom-3 -translate-x-1/2 inline-flex items-center justify-center rounded-full border border-border p-2 text-foreground bg-background shadow transition-[opacity,background-color] duration-150 ease-in-out opacity-0 pointer-events-none hover:bg-accent motion-reduce:transition-none",
            (scrolledAway || anchoredMessageId) &&
              "opacity-100 pointer-events-auto",
          )}
          aria-label={anchoredMessageId ? "Return to latest" : "Scroll to latest"}
          aria-hidden={!scrolledAway && !anchoredMessageId}
          tabIndex={scrolledAway || anchoredMessageId ? 0 : -1}
          onClick={jumpToLatest}
        >
          <ArrowDown size={16} />
        </button>
      </div>

      <div className="px-[clamp(0.5rem,4%,5rem)] pb-2" ref={promptSlotRef}>
        {taskPlan !== null && (
          <div className="pb-2">
            <TaskPlanCard plan={taskPlan} live={taskPlanLive} />
          </div>
        )}
        {pendingPromptCount > 0 ? (
          <ComposerPrompt
            userQuestionRequests={userQuestions.requests}
            answeringQuestionCalls={userQuestions.answering}
            userQuestionErrors={userQuestions.errors}
            onAnswerUserQuestions={userQuestions.answer}
            planApprovalRequests={planApprovals.requests}
            decidingPlanCalls={planApprovals.deciding}
            planApprovalErrors={planApprovals.errors}
            onPlanDecision={planApprovals.decide}
            onPlanCancel={planApprovals.cancel}
          />
        ) : (
          <>
            <QueueTray client={client} chatId={chat.id} active={activeTurnId !== null} />
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
            history={composerHistory}
            modelMenu={composerModelMenu}
            permissionMenu={composerPermissionMenu}
            network={composerNetwork}
            reasoning={composerReasoning}
            plugins={composerPlugins.plugins}
            slash={{
              options: composerPlugins.slashOptions,
              invoked: invokedSkills,
              onInvoke: (names) =>
                useComposerDrafts
                  .getState()
                  .setSkills(chat.id, [...invokedSkills, ...names]),
              onRemove: (name) =>
                useComposerDrafts.getState().setSkills(
                  chat.id,
                  invokedSkills.filter((skill) => skill !== name),
                ),
              loadPromptBody: composerPlugins.loadPromptBody,
              onCommand: runSlashCommand,
            }}
            images={composerImages}
            files={composerFiles}
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
            onQueue={onQueue}
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
          </>
        )}
      </div>
    </section>
  );
}
