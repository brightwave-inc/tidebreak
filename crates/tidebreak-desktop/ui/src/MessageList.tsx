import {
  memo,
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type MutableRefObject,
} from "react";
import { Wand2 } from "lucide-react";
import type { ReactNode, Ref, RefCallback, UIEvent } from "react";
import type {
  ApprovalGrantRung,
  ApiClient,
  AgentRun,
  AgentActivityHistoryEntry,
  AgentRunProgress,
  AgentRunTaskPlan,
  PendingFolderAccessRequest,
  PendingOutputWritebackRequest,
  ToolActionPreview,
  ToolResultPreview,
  ExecFileChangeSummary,
  MemoryRecord,
  ModelInfo,
} from "./api";
import { ApprovalCard, type GrantScopeName } from "./ApprovalCard";
import { AppCardList } from "./AppCard";
import { AssistantWorkingIndicator } from "./AssistantWorkingIndicator";
import { FolderAccessCard } from "./FolderAccessCard";
import type { FolderAccessDecision, OutputWritebackDecision } from "./host";
import { OutputWritebackCard } from "./OutputWritebackCard";
import { MessageMarkdown } from "./MessageMarkdown";
import { MessageFooter } from "./MessageFooter";
import { AssistantMessageBody } from "./AssistantMessageBody";
import { TranscriptSkeleton } from "./TranscriptSkeleton";
import { UserMessage } from "./UserMessage";
import { AssistantSources, type AssistantSource } from "./AssistantSources";
import { ThinkingAccordion } from "./ThinkingAccordion";
import { stripCitationDirectives } from "./citationDirectives";
import { MessageCitationsProvider } from "./InlineCitation";
import { McpAppCard } from "./McpAppCard";
import { PlanDecisionResultCard } from "./PlanDecisionResultCard";
import { UserQuestionsResultCard } from "./UserQuestionsResultCard";
import { OutputCardList } from "./OutputCard";
import { ToolCommandCard, type ToolCallStatus } from "./ToolCallCard";
import { ErrorBoundary } from "./ErrorBoundary";
import {
  ToolActivityGroup,
  ToolActivityUnavailable,
  toolActivitySignature,
} from "./ToolActivityGroup";
import { WelcomeState, type StarterPromptOptions } from "./WelcomeState";
import { isolatedCard } from "./PendingCard";
import type { TranscriptImageAttachment } from "./ImageAttachments";
import { TranscriptImageAttachments } from "./TranscriptImageAttachments";
import {
  TranscriptFileAttachments,
  type TranscriptFileAttachment,
} from "./TranscriptFileAttachments";
import { BackgroundAgentList } from "./BackgroundAgentList";
import { WebSearchProviderRequiredCard } from "./WebSearchProviderRequiredCard";
import {
  MessageWebSources,
  collectWebSources,
  type MessageWebSource,
} from "./MessageWebSources";
import { useSourceNav } from "./panel/SourceNav";
import { TurnFailureNotice } from "./TurnFailureNotice";
import {
  AnswerVersionNote,
  AnswerVersionPager,
  BranchButton,
  BranchNotice,
  EditButton,
  RegenerateControl,
  UserMessageEditor,
  regenerateStartsNewChatCopy,
  type TurnActions,
} from "./MessageActions";
import type {
  AnswerVersions,
  LatestTurnSideEffects,
} from "./ChatTranscriptPresentation";
import type {
  RendererRefusalSource,
  TurnFailureCategory,
} from "./generated/wire";
import { ChangeSummaryCard } from "./ChangeSummaryCard";
import {
  MemoryRememberedCard,
  type MemoryRememberedClient,
} from "./MemoryRememberedCard";
import { useStreamStalled } from "./useStreamStalled";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { Notice, NoticeRetryButton } from "@/components/ui/notice";

export type ChatMessage =
  | {
      id: string;
      role: "user";
      /** The turn this message opens. Absent until the server names it. */
      turnId?: string;
      text: string;
      images?: TranscriptImageAttachment[];
      files?: TranscriptFileAttachment[];
      invokedSkills?: readonly string[];
      voiceInputUsed?: boolean;
      createdAt?: string;
    }
  | {
      id: string;
      role: "assistant";
      /** The turn this answer belongs to. Absent until the server names it. */
      turnId?: string;
      text: string;
      sources: AssistantSource[];
      createdAt?: string;
      /** The provider's presentable reasoning summary for this step, if any. */
      reasoning?: string;
      /** Interrupted mid-stream and replaced; rendered dimmed until the
       *  authoritative transcript sweeps it. */
      superseded?: boolean;
    }
  /** `turnId` is set on the notice a stopped turn leaves. */
  | { id: string; role: "system"; text: string; turnId?: string }
  /** Durable marker that earlier conversation was compacted. */
  | { id: string; role: "compaction" }
  | {
      id: string;
      role: "refusal";
      category: string | null;
      partialOutput: boolean;
      source?: RendererRefusalSource;
    }
  | {
      id: string;
      role: "tool";
      callId: string;
      name: string;
      status: ToolCallStatus;
      /** Durable child identity retained by a hydrated spawn activity. */
      backgroundAgentRunId?: string;
      /** The tool's own closed view of what it is doing, when it has one. */
      preview?: ToolActionPreview | null;
      /** What the call produced, once it has produced anything. */
      result?: ToolResultPreview | null;
      /** Set when a retained projection no longer parses against this build. */
      resultUnreadable?: boolean;
    }
  | {
      id: string;
      role: "approval";
      callId: string;
      summary: string;
      preview?: ToolActionPreview | null;
      canApprove: boolean;
      canRemember: boolean;
      /** The Auto-mode judge is deciding; the card stays fully actionable. */
      autoJudging?: boolean;
      /** Complete standing-grant ladder the server will honor for this call. */
      grantRungs?: readonly ApprovalGrantRung[];
      resolved?: boolean;
    }
  | { id: string; role: "error"; text: string }
  | {
      id: string;
      role: "turn_failure";
      /** The turn that failed. */
      turnId?: string;
      category: TurnFailureCategory;
      detail?: string;
      model?: { id: string; provider: ModelInfo["provider"] };
      invokedSkills?: readonly string[];
      voiceInputUsed?: boolean;
    }
  | {
      id: string;
      role: "change_summary";
      turnId: string;
      files: ExecFileChangeSummary[];
      createdAt?: string;
    }
  | {
      id: string;
      role: "memory_proposals";
      turnId: string;
      records: MemoryRecord[];
      createdAt?: string;
    }
  /**
   * Where a branch's copied history ends. Placed by the transcript, never
   * stored: the session holds only real messages.
   */
  | { id: string; role: "branch_notice" };

/** The turn a retry reruns, and the notice that offers it. */
export type RetryableTurn = {
  /** The failure or stop notice that offers this retry. */
  noticeId: string;
  /** The turn the retry answers again, in place. */
  turnId: string;
};

/**
 * The retry the transcript currently offers, if any.
 *
 * Only a transcript that *ends* on a failure or a stop has one. An older one
 * keeps its explanation but loses its button: rerunning a turn from the middle
 * of a conversation the reader has since moved past is a footgun, and the
 * turns after it already answered whatever came next.
 *
 * A retry answers the same turn again rather than sending its message a second
 * time, so a string of retries never stacks copies of the question.
 */
export function retryableTurn(
  messages: readonly ChatMessage[],
): RetryableTurn | null {
  const notice = messages[messages.length - 1];
  if (notice?.role === "turn_failure" && notice.turnId) {
    return { noticeId: notice.id, turnId: notice.turnId };
  }
  if (
    notice?.role === "system" &&
    notice.text === TURN_CANCELLED_NOTICE &&
    notice.turnId
  ) {
    return { noticeId: notice.id, turnId: notice.turnId };
  }
  return null;
}

/**
 * The transcript a retry of `turnId` leaves while it runs: everything stays,
 * because the retry continues the turn. Only the notice of a turn that left
 * nothing else goes, since the answer arriving below says the same thing.
 */
export function withRetriedTurn(
  messages: ChatMessage[],
  turnId: string,
): ChatMessage[] {
  const opened = messages.findIndex(
    (message) => message.role === "user" && message.turnId === turnId,
  );
  if (opened < 0) return messages;
  const isNotice = (message: ChatMessage) =>
    "turnId" in message &&
    message.turnId === turnId &&
    (message.role === "turn_failure" ||
      (message.role === "system" && message.text === TURN_CANCELLED_NOTICE));
  // Tool activity carries no turn id here, so what the turn left is read by
  // position: anything after its message other than its notice.
  const leftSomething = messages
    .slice(opened + 1)
    .some((message) => message.role !== "user" && !isNotice(message));
  return leftSomething
    ? messages
    : messages.filter((message) => !isNotice(message));
}

type MessageListProps = {
  messages: ChatMessage[];
  /** Enables MCP App cards to fetch their call's result envelope. */
  chatId?: string;
  folderAccessRequests: PendingFolderAccessRequest[];
  outputWritebackRequests?: PendingOutputWritebackRequest[];
  /**
   * How many questions and plan approvals are parked on the reader. Their cards
   * stand in the composer's slot rather than the transcript, so the list never
   * renders them — it only needs to know a turn is waiting on someone, so that
   * an otherwise-empty chat isn't greeted and the Working indicator stays down.
   */
  pendingPromptCount?: number;
  nativeHost: boolean;
  nativeBusy: boolean;
  resolvingFolderCalls: Set<string>;
  folderAccessErrors: Record<string, string>;
  resolvingOutputWritebackCalls?: Set<string>;
  outputWritebackErrors?: Record<string, string>;
  decidingApprovalCalls: Set<string>;
  approvalErrors: Record<string, string>;
  /** How far a remembered approval reaches, for the card's labels. */
  grantScope?: GrantScopeName;
  backgroundAgentRuns?: AgentRun[];
  backgroundAgentRunsLoading?: boolean;
  backgroundAgentRunsError?: string | null;
  onRetryBackgroundAgentRuns?: () => void;
  onCancelBackgroundAgentRun?: (runId: string) => Promise<void>;
  onLoadBackgroundAgentActivity?: (
    runId: string,
  ) => Promise<AgentActivityHistoryEntry[]>;
  onLoadBackgroundAgentTaskPlan?: (
    runId: string,
  ) => Promise<AgentRunTaskPlan | null>;
  onLoadBackgroundAgentProgress?: (
    runId: string,
    afterSequence: number,
  ) => Promise<AgentRunProgress>;
  onOpenBackgroundAgent?: (runId: string) => void;
  onOpenOutput?: (outputId: string) => void;
  /** The connected client, so a background agent row can fetch debug info. */
  backgroundAgentClient?: ApiClient;
  busy: boolean;
  /** False while the socket is rebuilding already-seen journal history. */
  animateStreaming?: boolean;
  /**
   * Semantic compaction is running for the open turn. Prefer a visible
   * "Compacting conversation" status over the ordinary Working indicator.
   */
  compacting?: boolean;
  /** The live turn's stream has gone quiet — see [useStreamStalled]. */
  streamStalled?: boolean;
  /**
   * The session's stream cursor, read by the one leaf that needs it. Every
   * stream event advances it; handing it down as a prop re-rendered the whole
   * transcript on each one. Without it, `streamStalled` decides.
   */
  streamActivity?: StreamActivitySource;
  scrollRef: Ref<HTMLDivElement>;
  /** Attached to the transcript content column so growth can drive auto-follow. */
  contentRef?: RefCallback<HTMLDivElement>;
  /** Lift the trailing exchange into a pinned wrapper so a just-sent message
   *  lands near the top with room below for its streaming reply. */
  pinLastTurn?: boolean;
  onScroll: (event: UIEvent<HTMLDivElement>) => void;
  onApproval: (
    callId: string,
    decision: "approve" | "reject",
    grant: ApprovalGrantRung | null,
  ) => void;
  onFolderAccessDecision: (
    callId: string,
    decision: FolderAccessDecision,
  ) => void;
  onFolderAccessCancel: (callId: string, turnId: string) => void;
  onOutputWritebackDecision?: (
    callId: string,
    decision: OutputWritebackDecision,
  ) => void;
  onOutputWritebackCancel?: (callId: string, turnId: string) => void;
  onSelectPrompt?: (prompt: string, options?: StarterPromptOptions) => void;
  /** Resend the failed turn. Offered only on the transcript's newest failure. */
  onRetryTurn?: (turn: RetryableTurn) => void;
  /** The conversation has turns older than the ones in `messages`. */
  hasEarlierMessages?: boolean;
  /**
   * Put the page of turns before the held ones at the top of `messages`.
   * "Show earlier messages" reveals held turns first, then calls this.
   */
  onLoadEarlierMessages?: () => Promise<void>;
  hydrated?: boolean;
  imageClient?: Pick<ApiClient, "getChatImageAttachment">;
  executionConfigClient?: Pick<ApiClient, "getExecConfig">;
  changeClient?: Pick<
    ApiClient,
    "getFileChangePreview" | "undoFileChange" | "undoTurnFileChanges"
  >;
  memoryClient?: MemoryRememberedClient;
  /** Earlier answers, keyed by the turn shown in their place. */
  answerVersions?: AnswerVersions;
  /** What the latest settled turn did outside the conversation. */
  latestSideEffects?: LatestTurnSideEffects | null;
  /** Edit, regenerate, and branch. Absent for a transcript that only shows. */
  turnActions?: TurnActions;
  /** Where this conversation was branched from, when it is a branch. */
  branchOrigin?: BranchOrigin;
  /**
   * A message a search is pointing at. When the window of turns on screen
   * starts after it, the window reaches back to the turn that holds it, so
   * the message renders and can be scrolled to.
   */
  revealMessageId?: string | null;
  /**
   * A note that closes the transcript, such as the way back from an earlier
   * stretch of history a search opened.
   */
  trailingNotice?: ReactNode;
};

/** Where a branch came from, as its transcript shows it. */
export type BranchOrigin = {
  /** The original's title, or null when it has none. */
  title: string | null;
  /** When the branch was made. Transcript entries after it are the branch's own. */
  branchedAt: string;
  /** Open the original. Absent when it no longer exists. */
  onOpen?: () => void;
};

/**
 * What every row reads to decide its turn actions. One object per transcript
 * render, so a row re-renders when a turn setting changes, not per token.
 */
type TurnUi = {
  actions?: TurnActions;
  /** The latest settled turn, or null while one runs. */
  latestTurnId: string | null;
  /** The message Edit opens: the one that opens the latest turn. */
  editableMessageId: string | null;
  editingTurnId: string | null;
  setEditingTurnId: (turnId: string | null) => void;
  /** Version group of every turn with versions: the turn shown in its place. */
  versionGroups: ReadonlyMap<string, string>;
  answerVersions: AnswerVersions;
  selectedVersions: Readonly<Record<string, number>>;
  selectVersion: (group: string, index: number) => void;
  latestSideEffects: LatestTurnSideEffects | null;
  /**
   * The turn the transcript offers Try again for: it failed or was stopped,
   * and its notice's Try again continues it, so Regenerate is not offered
   * beside it.
   */
  retryTurnId: string | null;
};

const NO_ANSWER_VERSIONS: AnswerVersions = {};

// Defaults for the background-agent props feed the grouping memo below, so
// they are module constants: a fresh `[]` or `() => {}` per render would be a
// new dependency every time and the memo would never hold.
const NO_BACKGROUND_AGENT_RUNS: AgentRun[] = [];
const noRetryBackgroundAgentRuns = () => undefined;
const noCancelBackgroundAgentRun = async () => undefined;
const noLoadBackgroundAgentActivity = async (): Promise<
  AgentActivityHistoryEntry[]
> => [];
const noLoadBackgroundAgentTaskPlan = async (): Promise<null> => null;
const noLoadBackgroundAgentProgress = async (
  _runId: string,
  afterSequence: number,
): Promise<AgentRunProgress> => ({
  entries: [],
  nextSequence: afterSequence,
});

export function MessageList({
  messages,
  chatId,
  folderAccessRequests,
  outputWritebackRequests = [],
  pendingPromptCount = 0,
  nativeHost,
  nativeBusy,
  resolvingFolderCalls,
  folderAccessErrors,
  resolvingOutputWritebackCalls = new Set(),
  outputWritebackErrors = {},
  decidingApprovalCalls,
  approvalErrors,
  grantScope,
  backgroundAgentRuns = NO_BACKGROUND_AGENT_RUNS,
  backgroundAgentRunsLoading = false,
  backgroundAgentRunsError = null,
  onRetryBackgroundAgentRuns = noRetryBackgroundAgentRuns,
  onCancelBackgroundAgentRun = noCancelBackgroundAgentRun,
  onLoadBackgroundAgentActivity = noLoadBackgroundAgentActivity,
  onLoadBackgroundAgentTaskPlan = noLoadBackgroundAgentTaskPlan,
  onLoadBackgroundAgentProgress = noLoadBackgroundAgentProgress,
  onOpenBackgroundAgent,
  onOpenOutput,
  busy,
  animateStreaming = true,
  compacting = false,
  streamStalled = false,
  streamActivity,
  scrollRef,
  contentRef,
  pinLastTurn = false,
  onScroll,
  onApproval,
  onFolderAccessDecision,
  onFolderAccessCancel,
  onOutputWritebackDecision = () => undefined,
  onOutputWritebackCancel = () => undefined,
  onSelectPrompt,
  onRetryTurn,
  hasEarlierMessages = false,
  onLoadEarlierMessages,
  hydrated = true,
  imageClient,
  executionConfigClient,
  changeClient,
  memoryClient,
  backgroundAgentClient,
  answerVersions = NO_ANSWER_VERSIONS,
  latestSideEffects = null,
  turnActions,
  branchOrigin,
  revealMessageId = null,
  trailingNotice,
}: MessageListProps) {
  // Stable identity between renders so memoized rows only re-render when the
  // approval state itself changes, not on every streamed token.
  const approvalState = useMemo(
    () => ({ decidingApprovalCalls, approvalErrors, grantScope }),
    [decidingApprovalCalls, approvalErrors, grantScope],
  );
  const retry = useMemo(() => {
    if (!onRetryTurn || busy) return undefined;
    const turn = retryableTurn(messages);
    if (!turn) return undefined;
    return { noticeId: turn.noticeId, onRetry: () => onRetryTurn(turn) };
  }, [messages, onRetryTurn, busy]);
  const [editingTurnId, setEditingTurnId] = useState<string | null>(null);
  const [selectedVersions, setSelectedVersions] = useState<
    Record<string, number>
  >({});
  const latest = useMemo(() => latestTurn(messages, busy), [messages, busy]);
  const retryTurnId = useMemo(
    () => retryableTurn(messages)?.turnId ?? null,
    [messages],
  );
  const versionGroups = useMemo(
    () => answerVersionGroups(answerVersions),
    [answerVersions],
  );
  const selectVersion = useCallback((group: string, index: number) => {
    setSelectedVersions((current) => ({ ...current, [group]: index }));
  }, []);
  // A new answer to a turn shows first: a selection made before a regenerate
  // named an index into the old list.
  const [versionsSeen, setVersionsSeen] = useState(answerVersions);
  if (versionsSeen !== answerVersions) {
    setVersionsSeen(answerVersions);
    setSelectedVersions((current) => {
      const kept = Object.fromEntries(
        Object.entries(current).filter(
          ([group]) =>
            (answerVersions[group]?.length ?? 0) ===
            (versionsSeen[group]?.length ?? 0),
        ),
      );
      return Object.keys(kept).length === Object.keys(current).length
        ? current
        : kept;
    });
  }
  const turnUi = useMemo<TurnUi>(
    () => ({
      actions: turnActions,
      latestTurnId: latest.turnId,
      editableMessageId: latest.editableMessageId,
      editingTurnId,
      setEditingTurnId,
      versionGroups,
      answerVersions,
      selectedVersions,
      selectVersion,
      latestSideEffects,
      retryTurnId,
    }),
    [
      turnActions,
      latest,
      editingTurnId,
      versionGroups,
      answerVersions,
      selectedVersions,
      selectVersion,
      latestSideEffects,
      retryTurnId,
    ],
  );
  const backgroundAgents = useMemo(
    () => ({
      runs: backgroundAgentRuns,
      loading: backgroundAgentRunsLoading,
      error: backgroundAgentRunsError,
      retry: onRetryBackgroundAgentRuns,
      cancel: onCancelBackgroundAgentRun,
      loadActivity: onLoadBackgroundAgentActivity,
      loadTaskPlan: onLoadBackgroundAgentTaskPlan,
      loadProgress: onLoadBackgroundAgentProgress,
      open: onOpenBackgroundAgent,
      openOutput: onOpenOutput,
      client: backgroundAgentClient,
    }),
    [
      backgroundAgentRuns,
      backgroundAgentRunsLoading,
      backgroundAgentRunsError,
      onRetryBackgroundAgentRuns,
      onCancelBackgroundAgentRun,
      onLoadBackgroundAgentActivity,
      onLoadBackgroundAgentTaskPlan,
      onLoadBackgroundAgentProgress,
      onOpenBackgroundAgent,
      onOpenOutput,
      backgroundAgentClient,
    ],
  );
  // A long conversation opens on its newest turns. The window starts at the
  // user message that opens its oldest turn; it is set once the transcript
  // arrives, only ever reaches further back, and new turns join below it.
  const [windowStart, setWindowStart] = useState<{ id: string | null } | null>(
    null,
  );
  let start = windowStart;
  if (start === null && hydrated && messages.length > 0) {
    start = {
      id: turnWindowStart(messages, messages.length, TRANSCRIPT_TURNS_SHOWN),
    };
    setWindowStart(start);
  }
  const startIndex = start?.id
    ? Math.max(
        0,
        messages.findIndex((message) => message.id === start.id),
      )
    : 0;
  // A search pointing above the window pulls the window back to the turn
  // that holds its message. Only ever further back, like "Show earlier".
  if (revealMessageId !== null && startIndex > 0) {
    const target = messages.findIndex(
      (message) => message.id === revealMessageId,
    );
    if (target >= 0 && target < startIndex) {
      setWindowStart({ id: turnStartAtOrBefore(messages, target) });
    }
  }
  const visibleMessages = useMemo(() => {
    const windowed = startIndex > 0 ? messages.slice(startIndex) : messages;
    const shown = withSelectedVersions(
      windowed,
      answerVersions,
      selectedVersions,
    );
    return branchOrigin
      ? withBranchNotice(shown, branchOrigin.branchedAt, startIndex > 0)
      : shown;
  }, [messages, startIndex, answerVersions, selectedVersions, branchOrigin]);
  // Grouping walks the transcript and builds every row's element; memoized
  // so a render whose inputs are unchanged (a scroll, a pending-card flag)
  // reuses the rows instead of rebuilding a long conversation's worth. Each
  // grouping also hands its phases to the next, which reuses the ones a new
  // token did not touch.
  const phaseCache = useRef<ActivityPhaseCache | null>(null);
  const {
    items: messageItems,
    lastTurnStart,
    turnStarts,
  } = useMemo(() => {
    const grouped = groupMessageItems(
      visibleMessages,
      busy,
      animateStreaming,
      onApproval,
      approvalState,
      imageClient,
      chatId,
      changeClient,
      memoryClient,
      backgroundAgents,
      retry,
      phaseCache.current,
      turnUi,
      branchOrigin,
    );
    phaseCache.current = grouped.phases;
    return grouped;
  }, [
    visibleMessages,
    busy,
    animateStreaming,
    onApproval,
    approvalState,
    imageClient,
    chatId,
    changeClient,
    memoryClient,
    backgroundAgents,
    retry,
    turnUi,
    branchOrigin,
  ]);

  // The scroll viewport, for keeping the reader's place when earlier turns
  // appear above it. The caller's ref still gets the element.
  const scrollElement = useRef<HTMLDivElement | null>(null);
  const attachScroll = useCallback(
    (element: HTMLDivElement | null) => {
      scrollElement.current = element;
      if (typeof scrollRef === "function") scrollRef(element);
      else if (scrollRef) {
        (scrollRef as MutableRefObject<HTMLDivElement | null>).current =
          element;
      }
    },
    [scrollRef],
  );
  // Revealed turns land above what the reader is looking at. Holding their
  // distance from the bottom keeps that content where it was, whether or not
  // the webview anchors scrolling itself.
  const pendingRestore = useRef<{
    fromBottom: number;
    firstId: string | undefined;
  } | null>(null);
  const firstVisibleId = visibleMessages[0]?.id;
  useLayoutEffect(() => {
    const pending = pendingRestore.current;
    const scroller = scrollElement.current;
    if (!pending || !scroller || pending.firstId === firstVisibleId) return;
    pendingRestore.current = null;
    scroller.scrollTop = scroller.scrollHeight - pending.fromBottom;
  }, [firstVisibleId]);
  const [earlier, setEarlier] = useState<"idle" | "loading" | "failed">("idle");
  const showEarlier = () => {
    if (earlier === "loading") return;
    const scroller = scrollElement.current;
    pendingRestore.current = scroller
      ? {
          fromBottom: scroller.scrollHeight - scroller.scrollTop,
          firstId: firstVisibleId,
        }
      : null;
    if (startIndex > 0) {
      setWindowStart({
        id: turnWindowStart(messages, startIndex, TRANSCRIPT_TURNS_SHOWN),
      });
      return;
    }
    if (!onLoadEarlierMessages) return;
    // Everything held is on screen now, so the page that arrives is too.
    setWindowStart({ id: null });
    setEarlier("loading");
    onLoadEarlierMessages().then(
      () => setEarlier("idle"),
      () => {
        pendingRestore.current = null;
        setEarlier("failed");
      },
    );
  };
  const hasEarlier = startIndex > 0 || hasEarlierMessages;

  // Only greet a genuinely empty, fully-hydrated conversation. While an
  // existing chat's transcript is still loading it is transiently empty; showing
  // the welcome there would flash "How can I help?" before its history renders.
  const isEmpty =
    hydrated &&
    messages.length === 0 &&
    folderAccessRequests.length === 0 &&
    outputWritebackRequests.length === 0 &&
    pendingPromptCount === 0 &&
    !busy;

  if (isEmpty) {
    return (
      <div className="messages is-empty" ref={attachScroll} onScroll={onScroll}>
        <WelcomeState
          onSelectPrompt={onSelectPrompt}
          executionConfigClient={executionConfigClient}
        />
      </div>
    );
  }

  // A conversation that hasn't hydrated yet is transiently empty; a skeleton
  // holds the shape of a transcript so the pane doesn't flash blank before the
  // history lands.
  if (!hydrated && messages.length === 0) {
    return (
      <div className="messages" ref={attachScroll} onScroll={onScroll}>
        <div className="messages-column">
          <TranscriptSkeleton />
        </div>
      </div>
    );
  }

  // The continuation cards and the working indicator belong to the turn in
  // flight, so when the trailing turn is pinned they ride inside its wrapper.
  const pendingWorkCount =
    folderAccessRequests.length +
    outputWritebackRequests.length +
    pendingPromptCount;
  const trailing = (
    <>
      {folderAccessRequests.map((request) =>
        isolatedCard(
          `folder-access-${request.callId}`,
          `${resolvingFolderCalls.has(request.callId)} ${folderAccessErrors[request.callId] ?? ""}`,
          <FolderAccessCard
            request={request}
            nativeHost={nativeHost}
            nativeBusy={nativeBusy}
            working={resolvingFolderCalls.has(request.callId)}
            error={folderAccessErrors[request.callId]}
            onDecision={(decision) =>
              onFolderAccessDecision(request.callId, decision)
            }
            onCancel={() =>
              onFolderAccessCancel(request.callId, request.turnId)
            }
          />,
          request.callId,
        ),
      )}
      {outputWritebackRequests.map((request) =>
        isolatedCard(
          `output-writeback-${request.callId}`,
          `${resolvingOutputWritebackCalls.has(request.callId)} ${outputWritebackErrors[request.callId] ?? ""}`,
          <OutputWritebackCard
            request={request}
            nativeHost={nativeHost}
            working={resolvingOutputWritebackCalls.has(request.callId)}
            error={outputWritebackErrors[request.callId]}
            onDecision={(decision) =>
              onOutputWritebackDecision(request.callId, decision)
            }
            onCancel={() =>
              onOutputWritebackCancel(request.callId, request.turnId)
            }
          />,
          request.callId,
        ),
      )}
      {compacting ? (
        <AssistantWorkingIndicator compacting />
      ) : shouldShowAssistantWorking(messages, busy, pendingWorkCount) ? (
        <AssistantWorkingIndicator />
      ) : shouldShowAssistantWorking(messages, busy, pendingWorkCount, true) ? (
        <StalledStreamIndicator
          busy={busy}
          source={streamActivity}
          stalled={streamStalled}
        />
      ) : null}
    </>
  );

  const pin = pinLastTurn && lastTurnStart >= 0;

  // One box per turn. A settled turn — every one but the last — can skip
  // layout and paint while it is off screen (see `.message-turn.is-settled`).
  const bounds: { item: number; messageId: string | null }[] =
    turnStarts[0]?.item === 0
      ? turnStarts
      : [{ item: 0, messageId: null }, ...turnStarts];
  const turns = bounds.map((bound, index) => {
    const last = index === bounds.length - 1;
    const end = bounds[index + 1]?.item ?? messageItems.length;
    return (
      <div
        key={bound.messageId ? `turn-${bound.messageId}` : "turn-leading"}
        className={cn("message-turn", last ? pin && "is-pinned" : "is-settled")}
      >
        {messageItems.slice(bound.item, end)}
        {last && pin && trailing}
      </div>
    );
  });

  return (
    <div className="messages" ref={attachScroll} onScroll={onScroll}>
      <div className="messages-column" ref={contentRef}>
        {hasEarlier && (
          <EarlierMessagesControl
            loading={earlier === "loading"}
            failed={earlier === "failed"}
            onShow={showEarlier}
          />
        )}
        {turns}
        {!pin && trailing}
        {trailingNotice}
      </div>
    </div>
  );
}

/**
 * How many turns the transcript shows when it opens, and how many more each
 * "Show earlier messages" reveals. Opening a conversation renders this many
 * answers at once, so it is sized to keep that under a quarter second: forty
 * 5 KB answers measured about twice that. A transcript page holds twice as
 * many turns, so the first reveal needs no fetch.
 */
export const TRANSCRIPT_TURNS_SHOWN = 20;

/**
 * The message that opens the last `turns` turns before `end`, or null when
 * those turns reach the start of the transcript.
 */
export function turnWindowStart(
  messages: readonly ChatMessage[],
  end: number,
  turns: number,
): string | null {
  let seen = 0;
  for (let index = end - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message?.role !== "user") continue;
    seen += 1;
    if (seen === turns) return index > 0 ? message.id : null;
  }
  return null;
}

/**
 * The message that opens the turn holding `index`, as a window start: the
 * nearest user message at or before it, or null when that is the first
 * message, or there is none.
 */
export function turnStartAtOrBefore(
  messages: readonly ChatMessage[],
  index: number,
): string | null {
  for (let at = index; at >= 0; at -= 1) {
    const message = messages[at];
    if (message?.role === "user") return at > 0 ? message.id : null;
  }
  return null;
}

/**
 * The way to earlier turns, at the top of the transcript: first the ones held
 * but not shown, then the conversation's earlier pages.
 */
function EarlierMessagesControl({
  loading,
  failed,
  onShow,
}: {
  loading: boolean;
  failed: boolean;
  onShow: () => void;
}) {
  return (
    <div className="flex flex-col items-center gap-1.5 pb-2">
      <Button
        type="button"
        size="sm"
        variant="outline"
        disabled={loading}
        aria-busy={loading}
        onClick={onShow}
      >
        {loading && <Spinner />}
        Show earlier messages
      </Button>
      {failed && (
        <p className="text-muted-foreground text-xs" role="alert">
          Could not load earlier messages. Try again.
        </p>
      )}
    </div>
  );
}

type ToolMessage = Extract<ChatMessage, { role: "tool" }>;

/** Whether this message belongs to an activity phase rather than to the conversation. */
function isActivityMessage(message: ChatMessage | undefined): boolean {
  if (message === undefined) return false;
  return message.role === "tool" || message.role === "approval";
}

/**
 * Whether an assistant entry renders nothing at all: no prose, no sources, no
 * reasoning. The bubble component returns `null` for these, so nothing marks
 * their position on screen — which is exactly why they must not carry any
 * structural weight in grouping.
 */
function isInvisibleAssistant(message: ChatMessage | undefined): boolean {
  return (
    message !== undefined &&
    message.role === "assistant" &&
    !message.text &&
    message.sources.length === 0 &&
    !message.reasoning
  );
}

/**
 * Whether the assistant bubble at `index` closes its turn. A turn's prose is
 * split into one bubble per activity phase it passed through, and the copy
 * action and timestamp belong to the turn, not to each fragment — so only
 * the closing bubble carries them. Activity after a bubble is the turn
 * continuing, whatever it goes on to say; another assistant bubble right
 * behind it (a superseded stream's replacement) is the same turn resuming.
 */
export function isTurnClosingAssistant(
  messages: readonly ChatMessage[],
  index: number,
): boolean {
  const follower = messages[index + 1];
  if (isActivityMessage(follower)) return false;
  return follower === undefined || follower.role !== "assistant";
}

export function groupMessageItems(
  messages: ChatMessage[],
  busy: boolean,
  animateStreaming: boolean,
  onApproval: (
    callId: string,
    decision: "approve" | "reject",
    grant: ApprovalGrantRung | null,
  ) => void,
  approvalState?: {
    decidingApprovalCalls: Set<string>;
    approvalErrors: Record<string, string>;
    grantScope?: GrantScopeName;
  },
  imageClient?: Pick<ApiClient, "getChatImageAttachment">,
  chatId?: string,
  changeClient?: Pick<
    ApiClient,
    "getFileChangePreview" | "undoFileChange" | "undoTurnFileChanges"
  >,
  memoryClient?: MemoryRememberedClient,
  backgroundAgents: BackgroundAgentsContext = {
    runs: [],
    loading: false,
    error: null,
    retry: () => undefined,
    cancel: async () => undefined,
    loadActivity: async () => [],
    loadTaskPlan: async () => null,
    loadProgress: async (_runId: string, afterSequence: number) => ({
      entries: [],
      nextSequence: afterSequence,
    }),
  },
  retry?: { noticeId: string; onRetry: () => void },
  /**
   * The phases the last grouping built. A phase whose rows and inputs are all
   * unchanged is reused as the same element, so a streamed token re-renders
   * the phase it touched instead of every phase in the conversation.
   */
  previousPhases: ActivityPhaseCache | null = null,
  turnUi?: TurnUi,
  branchOrigin?: BranchOrigin,
) {
  const items: ReactNode[] = [];
  const nextPhases = new Map<string, BuiltPhase>();
  // The item index at which the trailing turn opens (its user message). Lets the
  // caller lift the last exchange into a pinned wrapper without re-deriving the
  // turn boundary. Stays -1 for a transcript that opens on activity alone.
  let lastTurnStart = -1;
  // Where every turn opens, with the message that opens it, so the caller can
  // give each turn its own box.
  const turnStarts: { item: number; messageId: string }[] = [];
  // Cards whose whole content is the situation, not the call — a standing
  // call-to-action the reader answers once. Parallel calls that all fail the
  // same way would otherwise stack identical copies of it. The claim is per
  // turn, not per transcript: the next turn hitting the same wall is a live
  // prompt again, so a user message clears it.
  let standingCardKeys = new Set<string>();
  // The pages this turn's searches found, gathered across every activity phase
  // it passed through and listed once, under the answer they fed. Reset at the
  // turn boundary: the previous turn's sources are not this answer's.
  let turnWebSources: MessageWebSource[] = [];
  let index = 0;
  let groupIndex = 0;
  let streamingAssistantId: string | undefined;
  if (busy) {
    for (
      let messageIndex = messages.length - 1;
      messageIndex >= 0;
      messageIndex -= 1
    ) {
      const candidate = messages[messageIndex];
      if (candidate?.role === "assistant" && !candidate.superseded) {
        streamingAssistantId = candidate.id;
        break;
      }
      // A newly submitted user message is busy before its turn-start event
      // arrives. Do not make the preceding completed assistant look live.
      if (candidate?.role === "user") break;
    }
  }

  // A turn with earlier answers shows its pager under the answer that closes
  // it. When no answer bubble closes it (the newest attempt failed or stopped
  // before saying anything), the pager gets a row of its own at the turn's end.
  let pagerOwed: string | null = null;
  const settlePager = () => {
    if (pagerOwed === null || !turnUi) return;
    const pager = versionPager(turnUi, pagerOwed);
    if (pager) {
      items.push(
        <div key={`versions-${pagerOwed}`} className="message-versions-row">
          <AnswerVersionPager {...pager} />
          {pager.index < pager.count - 1 && (
            <AnswerVersionNote latest={pager.count} />
          )}
        </div>,
      );
    }
    pagerOwed = null;
  };

  while (index < messages.length) {
    const message = messages[index];

    if (!isActivityMessage(message)) {
      if (message.role === "user") {
        settlePager();
        lastTurnStart = items.length;
        turnStarts.push({ item: items.length, messageId: message.id });
        standingCardKeys = new Set<string>();
        turnWebSources = [];
        if (turnUi && message.turnId) {
          pagerOwed = turnUi.versionGroups.get(message.turnId) ?? null;
        }
      }
      const closesTurn =
        message.role === "assistant" && isTurnClosingAssistant(messages, index);
      if (
        closesTurn &&
        pagerOwed !== null &&
        !isInvisibleAssistant(message) &&
        message.role === "assistant" &&
        message.turnId !== undefined &&
        turnUi?.versionGroups.get(message.turnId) === pagerOwed
      ) {
        pagerOwed = null;
      }
      items.push(
        <MessageBubble
          key={message.id}
          message={message}
          busy={message.id === streamingAssistantId}
          animateStreaming={animateStreaming}
          sequenceEnd={message.role !== "assistant" || closesTurn}
          imageClient={imageClient}
          chatId={chatId}
          changeClient={changeClient}
          memoryClient={memoryClient}
          onRetry={retry?.noticeId === message.id ? retry.onRetry : undefined}
          turnUi={turnUi}
          branchOrigin={
            message.role === "branch_notice" ? branchOrigin : undefined
          }
        />,
      );
      // A sibling of the bubble rather than part of it: the row is built from
      // the turn's tool rows, which the bubble knows nothing about, and keeping
      // it outside leaves the memoized bubble's props untouched.
      if (closesTurn && turnWebSources.length > 0) {
        items.push(
          <MessageWebSources
            key={`${message.id}-web-sources`}
            sources={turnWebSources}
          />,
        );
        turnWebSources = [];
      }
      index += 1;
      continue;
    }

    // One phase per contiguous run of activity, however long. A phase that
    // splits at every assistant sentence is not a phase.
    const phaseStart = index;
    index = activityPhaseEnd(messages, index);
    const firstCallId = phaseCallId(messages, phaseStart, index);
    // Keyed by the call that opened it rather than by position, so loading
    // earlier messages above does not remount every phase below.
    let key = `tool-activity-group-${firstCallId ?? `position-${groupIndex}`}`;
    if (nextPhases.has(key)) key = `${key}-${groupIndex}`;
    const standingBefore = [...standingCardKeys].sort().join(" ");
    const cached = previousPhases?.get(key);
    let built: BuiltPhase;
    if (
      cached !== undefined &&
      cached.groupIndex === groupIndex &&
      cached.animate === animateStreaming &&
      cached.chatId === chatId &&
      cached.standingBefore === standingBefore &&
      (cached.approvals === null ||
        (cached.approvals.onApproval === onApproval &&
          cached.approvals.approvalState === approvalState)) &&
      (cached.backgroundAgents === null ||
        cached.backgroundAgents === backgroundAgents) &&
      sameSpan(cached.span, messages, phaseStart, index)
    ) {
      // Nothing this phase draws on moved: hand React the element it already
      // has, which it skips without rendering anything inside it.
      built = cached;
      for (const claimed of cached.claimed) standingCardKeys.add(claimed);
    } else {
      built = buildActivityPhase({
        key,
        span: messages.slice(phaseStart, index),
        groupIndex,
        animate: animateStreaming,
        standingCardKeys,
        standingBefore,
        onApproval,
        approvalState,
        chatId,
        backgroundAgents,
      });
    }
    nextPhases.set(key, built);
    // Phases accumulate: a turn that searched, answered a little, and searched
    // again names every page it found under the answer that closes it.
    for (const source of built.webSources) {
      if (!turnWebSources.some((seen) => seen.url === source.url)) {
        turnWebSources.push(source);
      }
    }
    items.push(built.element);
    groupIndex += 1;
  }
  settlePager();

  return { items, lastTurnStart, turnStarts, phases: nextPhases };
}

/** The pager for one version group, or null when it has nothing to page. */
function versionPager(
  turnUi: TurnUi,
  group: string,
): { index: number; count: number; onSelect: (index: number) => void } | null {
  const earlier = turnUi.answerVersions[group];
  if (!earlier || earlier.length === 0) return null;
  const count = earlier.length + 1;
  const selected = turnUi.selectedVersions[group];
  return {
    index: selected === undefined ? count - 1 : Math.min(selected, count - 1),
    count,
    onSelect: (index) => turnUi.selectVersion(group, index),
  };
}

/**
 * The latest settled turn, and the message Edit opens on it.
 *
 * Nothing while a turn runs: every action waits for the conversation to
 * settle. Edit opens only the message that opens the turn, and only when no
 * guidance was sent after it: that later text is not what an edit reruns.
 */
export function latestTurn(
  messages: readonly ChatMessage[],
  busy: boolean,
): { turnId: string | null; editableMessageId: string | null } {
  if (busy) return { turnId: null, editableMessageId: null };
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message?.role !== "user") continue;
    if (!message.turnId) return { turnId: null, editableMessageId: null };
    const opener = messages.find(
      (candidate) =>
        candidate.role === "user" && candidate.turnId === message.turnId,
    );
    return {
      turnId: message.turnId,
      editableMessageId: opener?.id === message.id ? message.id : null,
    };
  }
  return { turnId: null, editableMessageId: null };
}

/** Every turn with earlier answers, and each version, mapped to its group. */
function answerVersionGroups(
  versions: AnswerVersions,
): ReadonlyMap<string, string> {
  const groups = new Map<string, string>();
  for (const [current, earlier] of Object.entries(versions)) {
    if (earlier.length === 0) continue;
    groups.set(current, current);
    for (const version of earlier) groups.set(version.turnId, current);
  }
  return groups;
}

/**
 * The transcript with each selected earlier answer in place of the answer it
 * was replaced by. The message stays; only what answered it changes.
 */
export function withSelectedVersions(
  messages: ChatMessage[],
  versions: AnswerVersions,
  selected: Readonly<Record<string, number>>,
): ChatMessage[] {
  const chosen = Object.entries(selected).filter(
    ([group, index]) => index < (versions[group]?.length ?? 0),
  );
  if (chosen.length === 0) return messages;
  const byGroup = new Map(chosen);
  const shown: ChatMessage[] = [];
  let index = 0;
  while (index < messages.length) {
    const message = messages[index];
    shown.push(message);
    index += 1;
    if (message.role !== "user" || !message.turnId) continue;
    const pick = byGroup.get(message.turnId);
    const version =
      pick === undefined ? undefined : versions[message.turnId]?.[pick];
    if (!version) continue;
    // Skip the current answer: everything up to the next turn's message.
    // Guidance sent during the turn is part of its answer, so it goes too.
    while (
      index < messages.length &&
      !(
        messages[index].role === "user" &&
        (messages[index] as { turnId?: string }).turnId !== message.turnId
      )
    ) {
      index += 1;
    }
    shown.push(...version.messages);
  }
  return shown;
}

/**
 * The transcript with a notice where a branch's copied history ends: before
 * the first message sent after the branch was made, or at the end when none
 * has been. `earlierHidden` holds the notice back when that point is above
 * what the transcript shows.
 */
export function withBranchNotice(
  messages: ChatMessage[],
  branchedAt: string,
  earlierHidden: boolean,
): ChatMessage[] {
  const branched = Date.parse(branchedAt);
  if (Number.isNaN(branched)) return messages;
  const at = messages.findIndex(
    (message) =>
      message.role === "user" &&
      message.createdAt !== undefined &&
      Date.parse(message.createdAt) > branched,
  );
  const notice: ChatMessage = { id: "branch-notice", role: "branch_notice" };
  if (at === -1) return [...messages, notice];
  if (at === 0 && earlierHidden) return messages;
  return [...messages.slice(0, at), notice, ...messages.slice(at)];
}

/** Where the activity phase that starts at `start` ends. */
function activityPhaseEnd(
  messages: readonly ChatMessage[],
  start: number,
): number {
  let index = start;
  while (index < messages.length) {
    if (isActivityMessage(messages[index])) {
      index += 1;
      continue;
    }
    // An assistant bubble that renders nothing must not end the phase: the
    // live reducer opens empty bubbles at turn-start and resume boundaries,
    // and the hydrated snapshot has no such entries — so a phase split here
    // would merge back when the turn settles, visibly reshuffling the
    // transcript. Approval resume cycles can stack several in a row, so the
    // whole run is swallowed when activity continues past it. A trailing
    // run is the response now streaming in, and stays outside the phase so
    // gaining its first characters does not move the group boundary.
    if (isInvisibleAssistant(messages[index])) {
      let ahead = index + 1;
      while (isInvisibleAssistant(messages[ahead])) ahead += 1;
      if (isActivityMessage(messages[ahead])) {
        index = ahead;
        continue;
      }
    }
    break;
  }
  return index;
}

/** The call id of the first activity in a phase, which names the phase. */
function phaseCallId(
  messages: readonly ChatMessage[],
  start: number,
  end: number,
): string | null {
  for (let index = start; index < end; index += 1) {
    const message = messages[index];
    if (message?.role === "tool" || message?.role === "approval") {
      return typeof message.callId === "string" ? message.callId : null;
    }
  }
  return null;
}

/** Whether `messages[start, end)` is exactly the rows `span` holds. */
function sameSpan(
  span: readonly ChatMessage[],
  messages: readonly ChatMessage[],
  start: number,
  end: number,
): boolean {
  if (span.length !== end - start) return false;
  for (let offset = 0; offset < span.length; offset += 1) {
    if (span[offset] !== messages[start + offset]) return false;
  }
  return true;
}

/** What a phase's background-agent list reads and calls. */
type BackgroundAgentsContext = {
  runs: AgentRun[];
  loading: boolean;
  error: string | null;
  retry: () => void;
  cancel: (runId: string) => Promise<void>;
  loadActivity: (runId: string) => Promise<AgentActivityHistoryEntry[]>;
  loadTaskPlan: (runId: string) => Promise<AgentRunTaskPlan | null>;
  loadProgress: (
    runId: string,
    afterSequence: number,
  ) => Promise<AgentRunProgress>;
  open?: (runId: string) => void;
  openOutput?: (outputId: string) => void;
  /** The connected client, so a row's "Copy debug info" can fetch its run. */
  client?: ApiClient;
};

type ApprovalState = {
  decidingApprovalCalls: Set<string>;
  approvalErrors: Record<string, string>;
  grantScope?: GrantScopeName;
};

/**
 * One activity phase as last built, with what it was built from. The next
 * grouping reuses the element while every input still matches.
 */
type BuiltPhase = {
  /** The transcript rows the phase covers, swallowed empty bubbles included. */
  span: readonly ChatMessage[];
  groupIndex: number;
  animate: boolean;
  chatId: string | undefined;
  /** The turn's claimed standing cards when the phase began. */
  standingBefore: string;
  /** Standing cards this phase claimed, replayed when it is reused. */
  claimed: readonly string[];
  /** Set only when an approval card in the phase reads them. */
  approvals: {
    onApproval: CardContext["onApproval"];
    approvalState: ApprovalState | undefined;
  } | null;
  /** Set only when the phase lists background agents. */
  backgroundAgents: BackgroundAgentsContext | null;
  webSources: readonly MessageWebSource[];
  element: ReactNode;
};

/** Phases from the last grouping, by key. */
export type ActivityPhaseCache = ReadonlyMap<string, BuiltPhase>;

function buildActivityPhase({
  key,
  span,
  groupIndex,
  animate,
  standingCardKeys,
  standingBefore,
  onApproval,
  approvalState,
  chatId,
  backgroundAgents,
}: {
  key: string;
  span: ChatMessage[];
  groupIndex: number;
  animate: boolean;
  standingCardKeys: Set<string>;
  standingBefore: string;
  onApproval: CardContext["onApproval"];
  approvalState: ApprovalState | undefined;
  chatId: string | undefined;
  backgroundAgents: BackgroundAgentsContext;
}): BuiltPhase {
  // Activity can resume through several assistant snapshots while remaining
  // one expandable phase. The collapsed live label should describe only the
  // latest snapshot; the expanded rail still preserves the whole phase.
  const phase: ChatMessage[] = [];
  let latestSnapshotStart = 0;
  for (const message of span) {
    if (isActivityMessage(message)) phase.push(message);
    else latestSnapshotStart = phase.length;
  }

  // A call parked on approval is represented by its approval card, so the
  // rail would otherwise announce the same pending action twice.
  const parked = new Set(
    phase.flatMap((entry) =>
      entry.role === "approval" && !entry.resolved ? [entry.callId] : [],
    ),
  );
  const activities = phase.filter(
    (entry): entry is ToolMessage =>
      entry.role === "tool" && !parked.has(entry.callId),
  );
  const latestActivities = phase
    .slice(latestSnapshotStart)
    .filter(
      (entry): entry is ToolMessage =>
        entry.role === "tool" && !parked.has(entry.callId),
    );
  const claimedBefore = new Set(standingCardKeys);
  const cards = surfacedCards(
    phase,
    parked,
    standingCardKeys,
    onApproval,
    approvalState,
    chatId,
  );
  const spawns = activities.flatMap((entry) =>
    entry.name === "spawn_sandbox_agent"
      ? [
          {
            callId: entry.callId,
            runId: entry.backgroundAgentRunId,
            status: entry.status,
          },
        ]
      : [],
  );
  const children: ReactNode[] = [...cards];
  if (spawns.length > 0) {
    children.push(
      isolatedCard(
        "background-agents",
        spawns.map((spawn) => spawn.status).join(" "),
        <BackgroundAgentList
          spawns={spawns}
          runs={backgroundAgents.runs}
          loading={backgroundAgents.loading}
          error={backgroundAgents.error}
          onRetry={backgroundAgents.retry}
          onCancel={backgroundAgents.cancel}
          onLoadActivity={backgroundAgents.loadActivity}
          onLoadTaskPlan={backgroundAgents.loadTaskPlan}
          onLoadProgress={backgroundAgents.loadProgress}
          onOpen={backgroundAgents.open}
          onOpenOutput={backgroundAgents.openOutput}
          {...(backgroundAgents.client && chatId
            ? { client: backgroundAgents.client, chatId }
            : {})}
        />,
      ),
    );
  }

  // The agent list below already names the delegation and every agent in it.
  // Leaving the spawn and wait calls on the rail as well stacks a second
  // summary of the same thing above it ("Waited for background agents and
  // delegated N tasks"), so the phase line covers everything except them.
  const railActivities =
    spawns.length > 0
      ? activities.filter(
          (entry) =>
            entry.name !== "spawn_sandbox_agent" &&
            entry.name !== "wait_for_agents",
        )
      : activities;
  const latestRailActivities =
    spawns.length > 0
      ? latestActivities.filter(
          (entry) =>
            entry.name !== "spawn_sandbox_agent" &&
            entry.name !== "wait_for_agents",
        )
      : latestActivities;
  const anchorIds = phase.flatMap((entry) =>
    entry.role === "tool" ? [entry.id] : [],
  );

  return {
    span,
    groupIndex,
    animate,
    chatId,
    standingBefore,
    claimed: [...standingCardKeys].filter((claim) => !claimedBefore.has(claim)),
    approvals: parked.size > 0 ? { onApproval, approvalState } : null,
    backgroundAgents: spawns.length > 0 ? backgroundAgents : null,
    webSources: collectWebSources(activities),
    // The rail and every card inside carry their own boundary, so this one is
    // only a backstop for the phase's own frame.
    element: (
      <ErrorBoundary key={key} fallback={<ToolActivityUnavailable />}>
        <ToolActivityGroup
          activities={railActivities}
          labelActivities={latestRailActivities}
          anchorIds={anchorIds}
          groupIndex={groupIndex}
          animate={animate}
          signature={`${toolActivitySignature(railActivities)}#${toolActivitySignature(latestRailActivities)}#${anchorIds.join(" ")}`}
        >
          {children.length > 0 ? children : undefined}
        </ToolActivityGroup>
      </ErrorBoundary>
    ),
  };
}

/** What deciding a card needs beyond the entry it is deciding about. */
type CardContext = {
  parked: Set<string>;
  /**
   * Keys of the standing call-to-action cards already shown this turn, so the
   * second call that fails the same way adds a rail row and nothing else.
   * Written through by {@link surfacedCard} as it claims a key.
   */
  standingCardKeys: Set<string>;
  onApproval: (
    callId: string,
    decision: "approve" | "reject",
    grant: ApprovalGrantRung | null,
  ) => void;
  approvalState?: {
    decidingApprovalCalls: Set<string>;
    approvalErrors: Record<string, string>;
    grantScope?: GrantScopeName;
  };
  chatId?: string;
};

/**
 * The cards that hang below a phase, always visible.
 *
 * A call earns one by having something a line of text can't carry: a command to
 * read, or a decision to make. Anything a card would show that the rail already
 * says stays in the rail.
 *
 * Total by construction. Each card's element is built inside its own `try`,
 * because this runs during the transcript's render, where a throw is not caught
 * by the boundaries the cards themselves carry — it escapes to the app-level
 * boundary and blanks the window. Catching around the whole list would at least
 * keep the app up, but it would still cost every card in the phase, including
 * the pending decision the turn is waiting on; per entry, a result the renderer
 * cannot make sense of costs only its own card.
 */
function surfacedCards(
  phase: ChatMessage[],
  parked: Set<string>,
  standingCardKeys: Set<string>,
  onApproval: (
    callId: string,
    decision: "approve" | "reject",
    grant: ApprovalGrantRung | null,
  ) => void,
  approvalState?: {
    decidingApprovalCalls: Set<string>;
    approvalErrors: Record<string, string>;
    grantScope?: GrantScopeName;
  },
  chatId?: string,
): ReactNode[] {
  const context: CardContext = {
    parked,
    standingCardKeys,
    onApproval,
    approvalState,
    chatId,
  };
  const cards: ReactNode[] = [];
  // In the order the calls happened, so the cards read as a sequence rather
  // than as two piles sorted by what kind of card they are.
  phase.forEach((entry, entryIndex) => {
    let card: ReactNode = null;
    let outputCards: ReactNode = null;
    let appCards: ReactNode = null;
    try {
      card = surfacedCard(entry, context);
      outputCards = surfacedOutputCards(entry);
      appCards = surfacedAppCards(entry);
    } catch (error) {
      console.error("tool result card could not be built", error);
      // The entry's own id may be the unreadable part, so the placeholder is
      // keyed on its position in the phase.
      card = <ToolActivityUnavailable key={`card-${entryIndex}`} />;
    }
    if (card !== null) cards.push(card);
    if (outputCards !== null) cards.push(outputCards);
    if (appCards !== null) cards.push(appCards);
  });
  return cards;
}

/**
 * The output cards an exec call earns, or `null` when it published nothing.
 *
 * Separate from {@link surfacedCard} because they are additive: the command
 * card says what ran, and these say what it produced — one clickable card per
 * created or updated output, surfaced at the end of the turn.
 */
function surfacedOutputCards(entry: ChatMessage): ReactNode {
  if (entry.role !== "tool" || entry.result?.tool !== "exec") return null;
  const outputs = entry.result.outputs ?? [];
  if (outputs.length === 0) return null;
  return isolatedCard(
    `${entry.id}-outputs`,
    outputs.map((output) => output.targetId ?? output.label).join(" "),
    <OutputCardList outputs={outputs} />,
  );
}

/**
 * The app cards a call earns, or `null` when it published none.
 *
 * Keyed on the row's kind rather than the tool's name: an app row is the
 * entries vocabulary saying "this is an app, and here is where it lives", and
 * a second tool that publishes one should get the same card without being
 * listed here.
 */
function surfacedAppCards(entry: ChatMessage): ReactNode {
  if (entry.role !== "tool" || entry.result?.tool !== "entries") return null;
  const apps = entry.result.entries.filter((row) => row.kind === "app");
  if (apps.length === 0) return null;
  return isolatedCard(
    `${entry.id}-apps`,
    apps.map((app) => app.targetId ?? app.label).join(" "),
    <AppCardList apps={apps} />,
  );
}

/** The card one entry earns, or `null` when it earns none. */
function surfacedCard(entry: ChatMessage, context: CardContext): ReactNode {
  const { parked, standingCardKeys, onApproval, approvalState, chatId } =
    context;
  if (entry.role === "approval") {
    if (entry.resolved) return null;
    return isolatedCard(
      entry.id,
      `${entry.canApprove} ${approvalState?.approvalErrors[entry.callId] ?? ""}`,
      <ApprovalCard
        callId={entry.callId}
        summary={entry.summary}
        preview={entry.preview ?? null}
        canApprove={entry.canApprove}
        canRemember={entry.canRemember}
        grantScope={approvalState?.grantScope ?? "chat"}
        autoJudging={entry.autoJudging ?? false}
        grantRungs={entry.grantRungs ?? []}
        deciding={
          approvalState?.decidingApprovalCalls.has(entry.callId) ?? false
        }
        error={approvalState?.approvalErrors[entry.callId]}
        onDecide={onApproval}
      />,
      entry.callId,
    );
  }
  if (entry.role !== "tool") return null;
  if (
    entry.result?.tool === "web_search_provider_required" &&
    !parked.has(entry.callId)
  ) {
    // The card says nothing about the call that produced it — it asks the
    // reader to configure a provider. Parallel searches all fail this way at
    // once, so only the first of them stands the card up; the rest are already
    // accounted for on the rail. A parked call renders no card at all and so
    // never claims the turn's slot.
    if (standingCardKeys.has("web_search_provider_required")) return null;
    standingCardKeys.add("web_search_provider_required");
    return isolatedCard(entry.id, "", <WebSearchProviderRequiredCard />);
  }
  // An MCP App view is keyed on the *result*: the tool has no action
  // preview, and its card exists to show what the server's declared view
  // renders — inside the sandbox — not to restate arguments or output.
  if (entry.result?.tool === "mcp_app" && !parked.has(entry.callId)) {
    return isolatedCard(
      entry.id,
      `${entry.result.server} ${entry.result.resourceUri}`,
      <McpAppCard
        server={entry.result.server}
        resourceUri={entry.result.resourceUri}
        chatId={chatId}
        callId={entry.callId}
      />,
    );
  }
  // A settled question round or plan decision is keyed on the *result*: the
  // card exists to say what the reader chose, which the arguments never held.
  // Still parked, the pinned card above the composer is asking the same thing.
  if (entry.result?.tool === "user_questions" && !parked.has(entry.callId)) {
    return isolatedCard(
      entry.id,
      "",
      <UserQuestionsResultCard
        answers={entry.result.answers}
        additionalContext={entry.result.additionalContext}
      />,
    );
  }
  if (entry.result?.tool === "plan_decision" && !parked.has(entry.callId)) {
    return isolatedCard(
      entry.id,
      "",
      <PlanDecisionResultCard
        title={entry.result.title}
        plan={entry.result.plan}
        accepted={entry.result.accepted}
        feedback={entry.result.feedback}
      />,
    );
  }
  // What a call found, read, or wrote renders inside the expanded rail,
  // under its own row — collapsed, a phase is one line, and a run of
  // searches must not stack a column of standing cards.
  // The approval card already shows this command and owns the decision.
  if (!entry.preview || parked.has(entry.callId)) return null;
  // A card earns its place by carrying something the rail cannot: a command
  // and its output. A search's query is fully said by the rail line and by
  // the approval card that asked about it, so it does not get an
  // exec-shaped card with tabs and an exit code.
  if (entry.preview.tool !== "exec") return null;
  return isolatedCard(
    entry.id,
    `${entry.status} ${entry.result?.tool ?? ""}`,
    <ToolCommandCard
      name={entry.name}
      status={entry.status}
      preview={entry.preview}
      result={entry.result?.tool === "exec" ? entry.result : null}
    />,
  );
}

/**
 * The skills a durable user message named, read back from the transcript.
 *
 * Read-only by construction: the message has already been sent, so there is
 * nothing to add or remove. It exists so a reader returning to a conversation
 * can still see what a turn was pointed at — the composer's own chips are
 * cleared with the text they were attached to.
 */
function TranscriptInvokedSkills({ skills }: { skills: readonly string[] }) {
  return (
    <ul
      className="m-0 mt-2 flex list-none flex-wrap gap-1.5 p-0"
      aria-label="Invoked skills"
    >
      {skills.map((name) => (
        <li
          key={name}
          className="inline-flex min-w-0 items-center gap-1.5 rounded-full border border-border bg-muted/50 px-2 py-0.5 text-muted-foreground"
        >
          <Wand2 size={12} aria-hidden="true" />
          <span className="max-w-[12rem] truncate text-xs font-medium">
            {name}
          </span>
        </li>
      ))}
    </ul>
  );
}

/**
 * Memoized row: settled messages keep referential identity across reducer
 * transitions, so during streaming only the live assistant bubble (whose
 * message object changes each token) re-renders.
 */
export const MessageBubble = memo(MessageBubbleImpl);

/** A stable stand-in for the roles that carry no citations. */
const EMPTY_SOURCES: readonly AssistantSource[] = [];

/**
 * One conversational turn. Tool calls and approvals are not turns — they belong
 * to an activity phase, which owns both the rail and the cards below it.
 */
function MessageBubbleImpl({
  message,
  busy,
  animateStreaming = true,
  sequenceEnd = true,
  imageClient,
  chatId,
  changeClient,
  memoryClient,
  onRetry,
  turnUi,
  branchOrigin,
}: {
  message: ChatMessage;
  busy: boolean;
  animateStreaming?: boolean;
  /** Only the turn-closing assistant bubble carries the footer. */
  sequenceEnd?: boolean;
  imageClient?: Pick<ApiClient, "getChatImageAttachment">;
  chatId?: string;
  changeClient?: Pick<
    ApiClient,
    "getFileChangePreview" | "undoFileChange" | "undoTurnFileChanges"
  >;
  memoryClient?: MemoryRememberedClient;
  /** Present only on the transcript's newest failure or stop notice. */
  onRetry?: () => void;
  /** The turn actions every row reads; absent for a history-only transcript. */
  turnUi?: TurnUi;
  /** Where the branch came from, on the notice that says so. */
  branchOrigin?: BranchOrigin;
}) {
  const sourceNav = useSourceNav();
  const richContentRef = useRef<HTMLDivElement | null>(null);
  // One way into the source panel for both anchors a citation has: the phrase
  // in the prose and the row at the foot of the message open the same place.
  const openSource = useMemo(
    () =>
      sourceNav
        ? (source: AssistantSource) =>
            sourceNav.openCitation({
              documentId: source.documentId,
              citationId: source.id,
            })
        : undefined,
    [sourceNav],
  );
  const sources =
    message.role === "assistant" ? message.sources : EMPTY_SOURCES;
  const citations = useMemo(
    () => ({ sources, onOpenSource: openSource }),
    [sources, openSource],
  );

  if (message.role === "assistant") {
    const reasoning = message.reasoning ?? "";
    // A bubble that holds only reasoning is a real transcript entry: it is what
    // the model did between two tool calls, or before the answer began.
    if (!message.text && message.sources.length === 0 && !reasoning)
      return null;

    if (message.superseded) {
      return (
        <article
          className="message message-assistant message-superseded"
          aria-label="Superseded response, replaced below"
        >
          <MessageMarkdown whole>{message.text}</MessageMarkdown>
        </article>
      );
    }

    const footer =
      sequenceEnd && !busy && turnUi && message.turnId
        ? answerFooter(turnUi, message.turnId)
        : null;
    return (
      <MessageCitationsProvider value={citations}>
        <article
          className="message message-assistant"
          aria-label="Assistant"
          data-message-id={message.id}
        >
          {reasoning && (
            <ThinkingAccordion
              text={reasoning}
              streaming={busy && !message.text}
            />
          )}
          {message.text && (
            <AssistantMessageBody
              text={message.text}
              streaming={busy && animateStreaming}
              // Only the live bubble can still grow; the rest parse whole.
              whole={!busy}
              containerRef={richContentRef}
            />
          )}
          <AssistantSources
            sources={message.sources}
            onOpenSource={openSource}
          />
          <MessageFooter
            role="assistant"
            // The clipboard yields what the message reads as, not how a
            // citation is stored.
            text={stripCitationDirectives(message.text)}
            createdAt={message.createdAt}
            settled={!busy}
            richContentRef={richContentRef}
            sequenceEnd={sequenceEnd}
            actions={footer?.actions}
            versions={footer?.versions}
            revealOnHover={footer?.revealOnHover}
          />
          {footer?.note}
        </article>
      </MessageCitationsProvider>
    );
  }

  if (message.role === "user") {
    const attachments = (
      <>
        {message.images &&
          message.images.length > 0 &&
          imageClient &&
          chatId &&
          isolatedCard(
            `${message.id}-images`,
            message.images.map((image) => image.attachmentId).join(" "),
            <TranscriptImageAttachments
              client={imageClient}
              chatId={chatId}
              images={message.images}
            />,
          )}
        {message.files &&
          message.files.length > 0 &&
          isolatedCard(
            `${message.id}-files`,
            message.files.map((file) => file.documentId).join(" "),
            <TranscriptFileAttachments files={message.files} />,
          )}
      </>
    );
    const turnId = message.turnId;
    const actions = turnUi?.actions;
    const editable =
      actions !== undefined &&
      turnId !== undefined &&
      message.id === turnUi?.editableMessageId;
    if (editable && turnUi?.editingTurnId === turnId) {
      const sideEffects =
        turnUi.latestSideEffects?.turnId === turnId
          ? turnUi.latestSideEffects.effects
          : [];
      return (
        <div className="message-user-frame">
          <UserMessageEditor
            text={message.text}
            attachments={attachments}
            sideEffects={sideEffects}
            onCancel={() => turnUi.setEditingTurnId(null)}
            onSubmit={(text) => {
              turnUi.setEditingTurnId(null);
              actions.onEdit(turnId, text);
            }}
          />
        </div>
      );
    }
    return (
      <UserMessage
        text={message.text}
        createdAt={message.createdAt}
        anchorId={message.id}
        leading={attachments}
        trailing={
          message.invokedSkills && message.invokedSkills.length > 0 ? (
            <TranscriptInvokedSkills skills={message.invokedSkills} />
          ) : null
        }
        copyable
        revealActionsOnHover
        actions={
          editable ? (
            <EditButton
              disabled={actions.pending}
              onEdit={() => turnUi?.setEditingTurnId(turnId)}
            />
          ) : undefined
        }
      />
    );
  }

  if (message.role === "branch_notice") {
    return branchOrigin ? (
      <BranchNotice title={branchOrigin.title} onOpen={branchOrigin.onOpen} />
    ) : null;
  }

  if (message.role === "system" || message.role === "error") {
    const error = message.role === "error";
    return (
      <Notice
        tone={error ? "critical" : "neutral"}
        density={error ? "default" : "compact"}
        className="self-stretch"
        action={
          onRetry && (
            <NoticeRetryButton size={error ? "sm" : "xs"} onClick={onRetry} />
          )
        }
      >
        {message.text}
      </Notice>
    );
  }

  if (message.role === "compaction") {
    return (
      <p
        className="self-stretch py-0.5 text-2xs tracking-wide text-muted-foreground"
        role="status"
      >
        Compacted conversation
      </p>
    );
  }

  if (message.role === "turn_failure") {
    return (
      <TurnFailureNotice
        category={message.category}
        detail={message.detail}
        model={message.model}
        onRetry={onRetry}
      />
    );
  }

  if (message.role === "change_summary") {
    if (!chatId || !changeClient) return null;
    return (
      <ChangeSummaryCard
        chatId={chatId}
        turnId={message.turnId}
        files={message.files}
        client={changeClient}
      />
    );
  }

  if (message.role === "memory_proposals") {
    if (!memoryClient) return null;
    return (
      <MemoryRememberedCard
        turnId={message.turnId}
        records={message.records}
        client={memoryClient}
      />
    );
  }

  if (message.role === "refusal") {
    return (
      <Notice tone="warning" density="compact" className="self-stretch">
        {refusalCopy(message.category, message.partialOutput, message.source)}
      </Notice>
    );
  }

  return null;
}

/**
 * The actions and pager under the answer that closes a turn.
 *
 * Every settled answer can be branched from. The latest one can also be
 * answered again, and its actions stay in view; older answers show theirs on
 * hover and focus.
 */
function answerFooter(
  turnUi: TurnUi,
  turnId: string,
): {
  actions?: ReactNode;
  versions?: ReactNode;
  note?: ReactNode;
  revealOnHover: boolean;
} {
  const group = turnUi.versionGroups.get(turnId) ?? turnId;
  const latest = group === turnUi.latestTurnId;
  const pager = turnUi.versionGroups.has(turnId)
    ? versionPager(turnUi, group)
    : null;
  const actions = turnUi.actions;
  const sideEffects =
    turnUi.latestSideEffects?.turnId === group
      ? turnUi.latestSideEffects.effects
      : [];
  return {
    revealOnHover: !latest,
    versions: pager ? <AnswerVersionPager {...pager} /> : undefined,
    note:
      pager && pager.index < pager.count - 1 ? (
        <AnswerVersionNote latest={pager.count} />
      ) : undefined,
    actions: actions ? (
      <>
        {/* A turn that stopped short is continued from its notice. */}
        {latest && group !== turnUi.retryTurnId && (
          <RegenerateControl
            onRegenerate={(model) => actions.onRegenerate(group, model)}
            retryModels={actions.retryModels}
            currentModelKey={actions.currentModelKey}
            disabled={actions.pending}
            newChatNote={regenerateStartsNewChatCopy(sideEffects)}
          />
        )}
        <BranchButton
          disabled={actions.pending}
          onBranch={() => actions.onBranch(turnId)}
        />
      </>
    ) : undefined,
  };
}

/**
 * The notice a stopped turn leaves in the transcript.
 *
 * One constant because two paths produce it — the live `turn_cancelled` event
 * and hydration from the durable snapshot — and they have to read identically
 * for a reopened conversation to look like the one the user left.
 */
export const TURN_CANCELLED_NOTICE = "Response cancelled";

/** Renderer-owned refusal copy; provider categories remain data, not prose. */
export function refusalCopy(
  category: string | null,
  partialOutput: boolean,
  source?: RendererRefusalSource,
): string {
  if (source === "report_blocked") {
    return "Tidebreak could not complete this task.";
  }
  const reason =
    (
      {
        cyber: "the cyber safety category",
        bio: "the biological safety category",
        frontier_llm: "the AI model-development policy category",
        reasoning_extraction: "the reasoning-extraction policy category",
        general_harms: "the general safety category",
      } as Record<string, string>
    )[category ?? ""] ??
    (category
      ? `the ${category.replaceAll("_", " ")} safety category`
      : "a safety policy");
  const explanation = `The model declined this response because it matched ${reason}.`;
  return partialOutput
    ? `The response above is incomplete. ${explanation}`
    : explanation;
}

/**
 * Where the live turn's stream activity can be read without re-rendering the
 * transcript: a cursor that advances with every applied stream event.
 */
export type StreamActivitySource = {
  subscribe: (onChange: () => void) => () => void;
  getSnapshot: () => number;
};

const subscribeToNothing = () => () => undefined;
const noActivity = () => 0;

/**
 * The Working indicator for a response that has started streaming, shown only
 * once the stream goes quiet.
 *
 * Its own leaf because the cursor it watches advances with every stream event:
 * subscribed here, an event re-renders this and nothing above it.
 */
function StalledStreamIndicator({
  busy,
  source,
  stalled,
}: {
  busy: boolean;
  source: StreamActivitySource | undefined;
  /** Decides when there is no source to watch. */
  stalled: boolean;
}) {
  const getActivity = source?.getSnapshot ?? noActivity;
  const activity = useSyncExternalStore(
    source?.subscribe ?? subscribeToNothing,
    getActivity,
    getActivity,
  );
  const quiet = useStreamStalled(busy, activity);
  return (source ? quiet : stalled) ? <AssistantWorkingIndicator /> : null;
}

/**
 * The generic worker indicator fills only gaps where no more specific live or
 * user-action status is already visible. All copy remains renderer-owned.
 *
 * A partial assistant response normally suppresses the indicator — the
 * streaming text is its own liveness signal. `streamStalled` reopens that one
 * gap: when the live stream has gone quiet mid-response, the indicator
 * returns under the partial text so a slow model reads differently from a
 * hung one, and hides again the moment deltas resume.
 */
export function shouldShowAssistantWorking(
  messages: readonly ChatMessage[],
  busy: boolean,
  pendingFolderAccessCount: number,
  streamStalled = false,
): boolean {
  if (!busy || pendingFolderAccessCount > 0) return false;
  const hasSpecificPendingStatus = messages.some(
    (message) =>
      (message.role === "tool" &&
        message.status !== "completed" &&
        message.status !== "failed" &&
        message.status !== "denied" &&
        message.status !== "cancelled") ||
      (message.role === "approval" && !message.resolved),
  );
  if (hasSpecificPendingStatus) return false;

  const latest = messages[messages.length - 1];
  if (latest?.role === "assistant") {
    return (
      streamStalled ||
      (latest.text.trim().length === 0 && latest.sources.length === 0)
    );
  }
  if (
    latest?.role === "system" ||
    latest?.role === "compaction" ||
    latest?.role === "error" ||
    latest?.role === "turn_failure"
  ) {
    return false;
  }
  return true;
}
