import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ComponentProps,
  type MutableRefObject,
  type ReactNode,
} from "react";
import type { ApiClient, Chat } from "./api";
import type { ContextUsageReading } from "./ContextUsageIndicator";
import { followScrollBehavior } from "./ChatScroll";
import { useTranscriptFollow } from "./useTranscriptFollow";
import { useChatSessionStore } from "./ChatSessionStore";
import { prependEarlierPage } from "./ChatSessionReducer";
import {
  approvalAnnouncement,
  composerTurnStatus,
  hostRequestAnnouncement,
  promptQuestion,
} from "./chatTurnStatus";
import { ChatPromptAnnouncer } from "./ChatPromptAnnouncer";
import {
  presentChatTranscript,
  TRANSCRIPT_PAGE_TURNS,
} from "./ChatTranscriptPresentation";
import {
  isOutlineMessage,
  isToolMessage,
  isUserMessage,
  stableSubset,
} from "./chatSessionSelectors";
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
  type ComposerPastedTexts,
  type ComposerProps,
  type ComposerSendBlocker,
  type ComposerSlash,
  type ComposerVoice,
} from "./Composer";
import type { SlashCommandName } from "./ComposerCommands";
import { ComposerPrompt } from "./ComposerPrompt";
import { ChatUsageDialog } from "./ChatUsageDialog";
import type {
  ComposerMemoryIncognito,
  ComposerNetwork,
  ComposerReasoning,
} from "./ComposerToolsMenu";
import {
  MessageList,
  type BranchOrigin,
  type RetryableTurn,
  type StreamActivitySource,
} from "./MessageList";
import type { TurnActions } from "./MessageActions";
import type { StarterPromptOptions } from "./WelcomeState";
import { revealPendingCall } from "./TranscriptFocus";
import { useTranscriptVisible } from "./TranscriptVisibility";
import { useFolderAccessRequests } from "./useFolderAccessRequests";
import { useOutputWritebackRequests } from "./useOutputWritebackRequests";
import { useToolApprovals } from "./useToolApprovals";
import { QueueTray, useChatQueueApi } from "./QueueTray";
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
import { useStableCallback } from "@/lib/useStableCallback";
import {
  TranscriptNavigation,
  transcriptNavigationEntries,
} from "./TranscriptNavigation";
import { messageWithPastedText } from "./PastedText";
import { EarlierHistoryNotice } from "./search/EarlierHistoryNotice";
import { TranscriptFindBar } from "./search/TranscriptFindBar";
import { TranscriptFindOverlay } from "./search/TranscriptFindOverlay";
import { useChatTranscriptSearch } from "./search/useChatTranscriptSearch";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyHeader,
  EmptyTitle,
  EmptyDescription,
  EmptyContent,
} from "@/components/ui/empty";

export type ChatViewProps = {
  client: ApiClient;
  chat: Chat;
  hydrated: boolean;
  hydrationError?: string | null;
  onRetryHydration?: () => void;
  nativeHost: boolean;
  deletingChat: boolean;
  composerModelMenu: ReactNode;
  composerPermissionMenu: ReactNode;
  /** Why the composer cannot send, such as no model being able to run. */
  composerSendBlocker?: ComposerSendBlocker | null;
  /** Context-window reading the composer shows beside its own controls. */
  contextUsage?: ContextUsageReading | null;
  composerNetwork?: ComposerNetwork;
  composerReasoning?: ComposerReasoning;
  composerMemoryIncognito?: ComposerMemoryIncognito;
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
  onSelectPrompt: (prompt: string, options?: StarterPromptOptions) => void;
  onSend: () => Promise<void>;
  /** Queue the draft to run after the active turn; absent disables queueing. */
  onQueue?: () => Promise<void>;
  /** Answer a failed or stopped turn again, in place. */
  onRetryTurn?: (turn: RetryableTurn) => void;
  /** Edit, regenerate, and branch on settled turns. */
  turnActions?: TurnActions;
  /** Where this conversation was branched from, when it is a branch. */
  branchOrigin?: BranchOrigin;
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
  hydrationError,
  onRetryHydration,
  nativeHost,
  deletingChat,
  composerModelMenu,
  composerPermissionMenu,
  composerSendBlocker = null,
  contextUsage,
  composerNetwork,
  composerReasoning,
  composerMemoryIncognito,
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
  turnActions,
  branchOrigin,
  onOpenAgentPanel,
  onOpenOutput,
}: ChatViewProps) {
  const transcriptVisible = useTranscriptVisible();
  const composerPlugins = useComposerPlugins(client);
  const composerAttachments = useComposerAttachments(chat.id);
  const invokedSkills = composerAttachments.skills;
  const pastedTexts = composerAttachments.pastedTexts;
  // What steering sends. The composer keeps it current as the reader types:
  // the pane itself does not subscribe to the draft, so a keystroke
  // re-renders the composer alone — never the transcript, and never the
  // panels beside it, where a document viewer that re-renders per keystroke
  // is one unstable dependency away from reloading its engine mid-typing.
  const steerDraftRef = useRef("");
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
    steerDraftRef,
    () => {
      onDraftChange("");
      // Pills go with the text they were attached to: accepted guidance has
      // already carried them, and leaving them behind would silently re-invoke
      // the same skills on whatever is typed next. Folder chips are the same
      // draft context; the grants stay on the chat.
      useComposerDrafts.getState().setSkills(chat.id, []);
      useComposerDrafts.getState().setFolders(chat.id, []);
      useComposerDrafts.getState().setPastedTexts(chat.id, []);
      onVoiceInputAccepted();
    },
    voiceInputUsed,
    invokedSkills,
  );
  // The hooks above hand back fresh closures on every render. The composer
  // and the transcript are memoized, so they get stable ones instead.
  const steer = useStableCallback(turnControls.steer);
  const stopTurn = useStableCallback(turnControls.cancel);
  const clearSteerFeedback = useStableCallback(turnControls.clearSteerFeedback);
  const decideApproval = useStableCallback(approvals.decide);
  const decideFolderAccess = useStableCallback(folderAccess.decide);
  const cancelFolderAccess = useStableCallback(folderAccess.cancel);
  const decideOutputWriteback = useStableCallback(outputWritebacks.decide);
  const cancelOutputWriteback = useStableCallback(outputWritebacks.cancel);
  // The pane subscribes to the turn's edges, not to its stream: the
  // transcript below reads the messages itself, so a streamed token renders
  // the transcript and nothing here.
  const busy = useChatSessionStore((session) => session.busy);
  const activeTurnId = useChatSessionStore((session) => session.activeTurnId);
  // What the composer's status region says about the turn. Both selectors
  // return strings, so a streamed token does not re-render the pane. Only a
  // running turn can be parked on an approval, so an idle chat skips the scan.
  const approvalWaiting = useChatSessionStore((session) =>
    session.busy ? approvalAnnouncement(session.messages) : "",
  );
  const lastTurnEnding = useChatSessionStore(
    (session) => session.lastTurnEnding,
  );
  const composerStatus = composerTurnStatus({
    waiting:
      approvalWaiting ||
      hostRequestAnnouncement(folderAccess.requests, outputWritebacks.requests),
    busy,
    ending: lastTurnEnding,
  });
  // A question or a plan replaces the composer and its status region, so a
  // region beside both announces it.
  const waitingPrompt = promptQuestion(
    userQuestions.requests,
    planApprovals.requests,
  );
  const chatQueue = useChatQueueApi(client, chat.id);
  // The questions asked and the tool calls made, as arrays that stay the same
  // object while a token streams into an answer.
  const selectOutline = useMemo(() => stableSubset(isOutlineMessage), []);
  const outline = useChatSessionStore((session) =>
    selectOutline(session.messages),
  );
  const selectUserMessages = useMemo(() => stableSubset(isUserMessage), []);
  const userMessages = selectUserMessages(outline);
  const selectToolCalls = useMemo(() => stableSubset(isToolMessage), []);
  const toolCalls = selectToolCalls(outline);
  const backgroundAgentSpawnKeys = useMemo(
    () => spawnKeysOf(toolCalls),
    [toolCalls],
  );
  const agentRuns = useAgentRuns(client, chat.id, backgroundAgentSpawnKeys);
  // Earlier turns arrive a page at a time, above the ones held, when the
  // reader asks for them.
  const hasEarlierMessages = useChatSessionStore(
    (session) => session.earlierCursor !== null,
  );
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const loadEarlierMessages = useCallback(async () => {
    const cursor = useChatSessionStore.getState().earlierCursor;
    if (cursor === null) return;
    const transcript = await client.listChatMessages(chat.id, {
      before: cursor,
      limit: TRANSCRIPT_PAGE_TURNS,
    });
    // The session store holds whichever conversation is open now.
    if (!mounted.current) return;
    const page = presentChatTranscript(transcript);
    useChatSessionStore
      .getState()
      .update((session) => prependEarlierPage(session, page, cursor));
  }, [chat.id, client]);
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
      recent: recentChatFiles(userMessages, files.items),
    }),
    [files, userMessages],
  );
  const composerPastedTexts: ComposerPastedTexts = useMemo(
    () => ({
      items: pastedTexts,
      onPaste: (text) => {
        clearSteerFeedback();
        const current =
          useComposerDrafts.getState().attachments[chat.id]?.pastedTexts ?? [];
        useComposerDrafts
          .getState()
          .setPastedTexts(chat.id, [
            ...current,
            { id: crypto.randomUUID(), text },
          ]);
      },
      onRemove: (id) => {
        clearSteerFeedback();
        const current =
          useComposerDrafts.getState().attachments[chat.id]?.pastedTexts ?? [];
        useComposerDrafts.getState().setPastedTexts(
          chat.id,
          current.filter((item) => item.id !== id),
        );
      },
    }),
    [chat.id, clearSteerFeedback, pastedTexts],
  );
  const composerHistory = useMemo(
    () =>
      userMessages
        .flatMap((message) => (message.text.trim() ? [message.text] : []))
        .reverse(),
    [userMessages],
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
        toast.error(
          "Wait for the current response to finish before compacting.",
        );
        return;
      }
      if (compactRequested) return;
      setCompactRequested(true);
      try {
        const run = await client.compactChat(chat.id, focus || undefined);
        if (run.compacted) {
          toast.success("Summarized the earlier part of this conversation");
        } else {
          toast.message(
            "Nothing to compact yet — this conversation still fits.",
          );
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
  const composerSlash: ComposerSlash = useMemo(
    () => ({
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
    }),
    [
      chat.id,
      composerPlugins.loadPromptBody,
      composerPlugins.slashOptions,
      invokedSkills,
      runSlashCommand,
    ],
  );
  const pluginItems = composerPlugins.plugins?.items;
  const selectPlugin = useStableCallback(
    (plugin: NonNullable<ComposerProps["plugins"]>["items"][number]) =>
      composerPlugins.plugins?.onSelect(plugin),
  );
  const plugins: ComposerProps["plugins"] = useMemo(
    () =>
      pluginItems ? { items: pluginItems, onSelect: selectPlugin } : undefined,
    [pluginItems, selectPlugin],
  );
  // Typing retires the verdict on the last piece of guidance. Accepted
  // guidance clears the draft through the raw callback instead, so "Guidance
  // sent" survives the clearing it caused.
  const changeDraft = useStableCallback((value: string) => {
    clearSteerFeedback();
    onDraftChange(value);
  });

  const navigate = useNavigate();
  const search = useSearch({ strict: false }) as {
    focus?: string;
    at?: string;
  };
  const focusCallId = search.focus;
  const anchoredMessageId = search.at;
  // The outline holds exactly the rows the rail presents, and keeps its
  // identity while an answer streams, so the rail's observers stay mounted
  // until a question or a tool call actually changes.
  const navigationEntries = useMemo(
    () => transcriptNavigationEntries(outline),
    [outline],
  );

  /** The composer's slot, which a pending question or plan card takes over. */
  const promptSlotRef = useRef<HTMLDivElement | null>(null);
  const visibleContinuationCallIdsRef = useRef<Set<string>>(new Set());
  const {
    scrollRef: attachScrollRef,
    contentRef: attachContentRef,
    onScroll: handleScroll,
    scrolledAway,
    fadeClass,
    scrollToBottom,
    armFollow,
    disarmFollow,
    isFollowing,
    scrollElement,
    viewportRef,
    beginProgrammaticScroll,
    endProgrammaticScroll,
    requestSmoothFollow,
  } = useTranscriptFollow({ visible: transcriptVisible });
  // True once the reader has sent in this mounted session. Gates the turn pin so
  // a freshly loaded history reads normally, then the just-sent turn is held tall
  // enough to land near the top of the viewport.
  const [pinLastTurn, setPinLastTurn] = useState(false);
  // Find in this conversation, and the palette's jumps into it. A message
  // on a page the transcript does not hold loads with that one page.
  const transcriptSearch = useChatTranscriptSearch({
    client,
    chatId: chat.id,
    memoryIncognito: chat.memory_incognito,
    hydrated,
    scrollElement,
    disarmFollow,
    beginProgrammaticScroll,
    endProgrammaticScroll,
  });

  // A conversation starts live at its tail. Streaming growth itself is handled
  // only by the content ResizeObserver in the follow hook; reacting to every
  // message-array replacement as well makes token updates repeatedly restart
  // scrolling and can fight a reader who is trying to move upward.
  useEffect(() => {
    armFollow();
  }, [chat.id, armFollow]);

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
    if (isFollowing()) scrollToBottom(followScrollBehavior(false));
  }, [
    folderAccess.requests,
    outputWritebacks.requests,
    userQuestions.requests,
    isFollowing,
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
      if (revealPendingCall(viewportRef.current, focusCallId)) {
        // Arriving at a specific card means the reader is no longer following
        // the end of the transcript; letting follow stay armed would scroll them
        // straight back off it.
        disarmFollow();
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
  }, [
    focusCallId,
    transcriptVisible,
    chat.id,
    disarmFollow,
    navigate,
    viewportRef,
  ]);

  // The router's hash is already occupied by hash history, so a rail jump is
  // represented by `?at=`. Keeping it in the URL makes an anchored reload land
  // in the same place; it is cleared only when the reader returns to the tail.
  useEffect(() => {
    if (!anchoredMessageId || !transcriptVisible || !scrollElement) return;
    const frame = window.requestAnimationFrame(() => {
      const target = Array.from(
        scrollElement.querySelectorAll<HTMLElement>("[data-transcript-anchor]"),
      ).find(
        (element) => element.dataset.transcriptAnchor === anchoredMessageId,
      );
      if (!target) return;
      disarmFollow();
      beginProgrammaticScroll();
      const containerRect = scrollElement.getBoundingClientRect();
      const targetRect = target.getBoundingClientRect();
      scrollElement.scrollTo({
        top: Math.max(
          0,
          scrollElement.scrollTop + targetRect.top - containerRect.top - 24,
        ),
        behavior: "smooth",
      });
      window.setTimeout(endProgrammaticScroll, 800);
    });
    return () => window.cancelAnimationFrame(frame);
  }, [
    anchoredMessageId,
    beginProgrammaticScroll,
    disarmFollow,
    endProgrammaticScroll,
    // The anchors are questions and tool calls; retry when those arrive.
    outline,
    scrollElement,
    transcriptVisible,
  ]);

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
    transcriptSearch.leaveHistory();
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
    armFollow(followScrollBehavior(false));
  }, [
    anchoredMessageId,
    armFollow,
    chat.id,
    navigate,
    transcriptSearch.leaveHistory,
  ]);

  const handleSend = useCallback(async () => {
    setPinLastTurn(true);
    // A turn lands at the end of the conversation, so a reader reading an
    // earlier stretch goes back to it.
    transcriptSearch.leaveHistory();
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
    armFollow();
    requestSmoothFollow();
    await onSend();
  }, [
    anchoredMessageId,
    armFollow,
    chat.id,
    navigate,
    onSend,
    requestSmoothFollow,
    transcriptSearch.leaveHistory,
  ]);

  const historyView = transcriptSearch.historyView;
  const findBar = transcriptSearch.find;
  const transcriptProps: ChatTranscriptProps = {
    chatId: chat.id,
    folderAccessRequests: folderAccess.requests,
    outputWritebackRequests: outputWritebacks.requests,
    pendingPromptCount,
    nativeHost,
    nativeBusy: folderAccess.resolving.size > 0,
    resolvingFolderCalls: folderAccess.resolving,
    folderAccessErrors: folderAccess.errors,
    resolvingOutputWritebackCalls: outputWritebacks.resolving,
    outputWritebackErrors: outputWritebacks.errors,
    decidingApprovalCalls: approvals.deciding,
    approvalErrors: approvals.errors,
    grantScope: chat.project_id ? "project" : "chat",
    backgroundAgentRuns: agentRuns.runs,
    backgroundAgentRunsLoading: agentRuns.loading,
    backgroundAgentRunsError: agentRuns.error,
    onRetryBackgroundAgentRuns: agentRuns.refresh,
    onCancelBackgroundAgentRun: agentRuns.cancel,
    onLoadBackgroundAgentActivity: agentRuns.loadActivity,
    onLoadBackgroundAgentTaskPlan: agentRuns.loadTaskPlan,
    onLoadBackgroundAgentProgress: agentRuns.loadProgress,
    onOpenBackgroundAgent: onOpenAgentPanel,
    onOpenOutput,
    backgroundAgentClient: client,
    scrollRef: attachScrollRef,
    contentRef: attachContentRef,
    pinLastTurn,
    onScroll: handleScroll,
    onApproval: decideApproval,
    onFolderAccessDecision: decideFolderAccess,
    onFolderAccessCancel: cancelFolderAccess,
    onOutputWritebackDecision: decideOutputWriteback,
    onOutputWritebackCancel: cancelOutputWriteback,
    onSelectPrompt,
    onRetryTurn,
    turnActions,
    branchOrigin,
    hasEarlierMessages,
    onLoadEarlierMessages: loadEarlierMessages,
    hydrated,
    imageClient: client,
    executionConfigClient: client,
    changeClient: client,
    memoryClient: client,
    revealMessageId: transcriptSearch.revealMessageId,
  };

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
      <div
        className={cn("message-view", fadeClass)}
        onFocusCapture={findBar.activate}
        onPointerDownCapture={findBar.activate}
      >
        {findBar.open && (
          <TranscriptFindOverlay scrollElement={scrollElement}>
            <TranscriptFindBar
              ref={findBar.inputRef}
              className="pointer-events-auto"
              query={findBar.query}
              onQueryChange={findBar.setQuery}
              state={findBar.state}
              onOlder={findBar.older}
              onNewer={findBar.newer}
              onClose={findBar.close}
            />
          </TranscriptFindOverlay>
        )}
        {hydrationError ? (
          <Empty role="alert" className="h-full">
            <EmptyHeader>
              <EmptyTitle>Could not load this conversation</EmptyTitle>
              <EmptyDescription className="break-words [overflow-wrap:anywhere]">
                {hydrationError}
              </EmptyDescription>
            </EmptyHeader>
            <EmptyContent>
              <Button variant="outline" onClick={onRetryHydration}>
                Try again
              </Button>
            </EmptyContent>
          </Empty>
        ) : historyView ? (
          <MessageList
            {...transcriptProps}
            key="history"
            messages={historyView.messages}
            answerVersions={historyView.answerVersions}
            busy={false}
            animateStreaming={false}
            folderAccessRequests={[]}
            outputWritebackRequests={[]}
            pendingPromptCount={0}
            pinLastTurn={false}
            turnActions={undefined}
            onRetryTurn={undefined}
            hasEarlierMessages={historyView.earlierCursor !== null}
            onLoadEarlierMessages={transcriptSearch.showEarlierHistory}
            trailingNotice={<EarlierHistoryNotice onLeave={jumpToLatest} />}
          />
        ) : (
          <ChatTranscript {...transcriptProps} />
        )}
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
            (scrolledAway || anchoredMessageId || historyView) &&
              "opacity-100 pointer-events-auto",
          )}
          aria-label={
            anchoredMessageId || historyView
              ? "Return to latest"
              : "Scroll to latest"
          }
          aria-hidden={!scrolledAway && !anchoredMessageId && !historyView}
          tabIndex={scrolledAway || anchoredMessageId || historyView ? 0 : -1}
          onClick={jumpToLatest}
        >
          <ArrowDown size={16} />
        </button>
      </div>

      <div className="px-[clamp(0.5rem,4%,5rem)] pb-2" ref={promptSlotRef}>
        <ChatPromptAnnouncer question={waitingPrompt} />
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
            <QueueTray
              queue={chatQueue}
              active={activeTurnId !== null}
              onStop={stopTurn}
            />
            <ChatComposer
              chatId={chat.id}
              steerDraftRef={steerDraftRef}
              activeTurnId={activeTurnId}
              busy={busy}
              cancelError={turnControls.cancelError}
              cancelPending={
                activeTurnId !== null &&
                turnControls.cancelPendingTurnId === activeTurnId
              }
              disabled={deletingChat || !hydrated}
              history={composerHistory}
              modelMenu={composerModelMenu}
              permissionMenu={composerPermissionMenu}
              contextUsage={contextUsage}
              network={composerNetwork}
              reasoning={composerReasoning}
              memoryIncognito={composerMemoryIncognito}
              plugins={plugins}
              slash={composerSlash}
              images={composerImages}
              files={composerFiles}
              pastedTexts={composerPastedTexts}
              folders={folders}
              voice={voice}
              nativeDropTarget={nativeDropTarget}
              attachError={attachError}
              onDraftChange={changeDraft}
              onSend={handleSend}
              onQueue={onQueue}
              onSteer={steer}
              onStop={stopTurn}
              resetKey={chat.id}
              steerError={turnControls.steerError}
              steerPending={
                activeTurnId !== null &&
                turnControls.steerPendingTurnId === activeTurnId
              }
              steerStatus={turnControls.steerStatus}
              turnStatus={composerStatus}
              sendBlocker={composerSendBlocker}
            />
          </>
        )}
      </div>
    </section>
  );
}

/** The session's stream cursor, for the transcript leaf that watches it. */
const chatStreamActivity: StreamActivitySource = {
  subscribe: (onChange) => useChatSessionStore.subscribe(onChange),
  getSnapshot: () => useChatSessionStore.getState().lastSeq,
};

type ChatTranscriptProps = Omit<
  ComponentProps<typeof MessageList>,
  | "messages"
  | "busy"
  | "animateStreaming"
  | "compacting"
  | "streamStalled"
  | "streamActivity"
  | "answerVersions"
  | "latestSideEffects"
>;

/**
 * The transcript, subscribed to the stream on its own: a streamed token
 * re-renders this and the rows the token touched, never the pane around it.
 */
const ChatTranscript = memo(function ChatTranscript(
  props: ChatTranscriptProps,
) {
  const messages = useChatSessionStore((session) => session.messages);
  const busy = useChatSessionStore((session) => session.busy);
  const animateStreaming = useChatSessionStore(
    (session) => session.animateStreaming,
  );
  const compacting = useChatSessionStore((session) => session.compacting);
  const answerVersions = useChatSessionStore(
    (session) => session.answerVersions,
  );
  const latestSideEffects = useChatSessionStore(
    (session) => session.latestSideEffects,
  );
  return (
    <MessageList
      {...props}
      messages={messages}
      busy={busy}
      animateStreaming={animateStreaming}
      compacting={compacting}
      streamActivity={chatStreamActivity}
      answerVersions={answerVersions}
      latestSideEffects={latestSideEffects}
    />
  );
});

/**
 * The composer, subscribed to the draft on its own: a keystroke re-renders
 * this and the composer, never the pane around them. It also keeps the steer
 * text current, because steering reads it outside a render.
 */
const ChatComposer = memo(function ChatComposer({
  chatId,
  steerDraftRef,
  ...props
}: Omit<ComposerProps, "draft"> & {
  chatId: string;
  steerDraftRef: MutableRefObject<string>;
}) {
  const draft = useComposerDraft(chatId);
  steerDraftRef.current = messageWithPastedText(
    draft,
    props.pastedTexts?.items ?? [],
  );
  return <Composer draft={draft} {...props} />;
});
