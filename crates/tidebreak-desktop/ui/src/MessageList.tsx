import { memo, useMemo, useRef } from "react";
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
import { TurnFailureNotice, turnFailureOffersRetry } from "./TurnFailureNotice";
import type { TurnFailureCategory } from "./generated/wire";
import { ChangeSummaryCard } from "./ChangeSummaryCard";

export type ChatMessage =
  | {
      id: string;
      role: "user";
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
      text: string;
      sources: AssistantSource[];
      createdAt?: string;
      /** The provider's presentable reasoning summary for this step, if any. */
      reasoning?: string;
      /** Interrupted mid-stream and replaced; rendered dimmed until the
       *  authoritative transcript sweeps it. */
      superseded?: boolean;
    }
  | { id: string; role: "system"; text: string }
  /** Durable marker that earlier conversation was compacted. */
  | { id: string; role: "compaction" }
  | {
      id: string;
      role: "refusal";
      category: string | null;
      partialOutput: boolean;
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
    };

/** Everything a retry needs to put the failed turn back on the wire unchanged. */
export type RetryableTurn = {
  /** The failure notice that offers this retry. */
  failureId: string;
  text: string;
  images: readonly TranscriptImageAttachment[];
  files: readonly TranscriptFileAttachment[];
  invokedSkills: readonly string[];
  voiceInputUsed: boolean;
};

/**
 * The retry the transcript currently offers, if any.
 *
 * Only a transcript that *ends* on a retryable failure has one. An older
 * failure keeps its explanation but loses its button: resending a prompt from
 * the middle of a conversation the reader has since moved past is a footgun,
 * and the turns after it already answered whatever came next.
 */
export function retryableTurn(
  messages: readonly ChatMessage[],
): RetryableTurn | null {
  const failure = messages[messages.length - 1];
  if (failure?.role !== "turn_failure") return null;
  if (!turnFailureOffersRetry(failure.category)) return null;
  for (let index = messages.length - 2; index >= 0; index -= 1) {
    const message = messages[index];
    if (message?.role !== "user") continue;
    // Nothing to resend — the prompt the turn failed on is no longer in hand.
    if (message.text.trim().length === 0) return null;
    return {
      failureId: failure.id,
      text: message.text,
      images: message.images ?? [],
      files: message.files ?? [],
      // The message being resent speaks for itself. It is not always the turn's
      // opening prompt — guidance sent mid-turn becomes a user message too, and
      // it carries its own invocation — so preferring the turn-level list here
      // would resend one message's text under another's skills.
      invokedSkills: message.invokedSkills ?? failure.invokedSkills ?? [],
      voiceInputUsed: failure.voiceInputUsed ?? message.voiceInputUsed ?? false,
    };
  }
  return null;
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
  hydrated?: boolean;
  imageClient?: Pick<ApiClient, "getChatImageAttachment">;
  executionConfigClient?: Pick<ApiClient, "getExecConfig">;
  changeClient?: Pick<
    ApiClient,
    "getFileChangePreview" | "undoFileChange" | "undoTurnFileChanges"
  >;
};

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
  backgroundAgentRuns = [],
  backgroundAgentRunsLoading = false,
  backgroundAgentRunsError = null,
  onRetryBackgroundAgentRuns = () => undefined,
  onCancelBackgroundAgentRun = async () => undefined,
  onLoadBackgroundAgentActivity = async () => [],
  onLoadBackgroundAgentTaskPlan = async () => null,
  onLoadBackgroundAgentProgress = async (_runId, afterSequence) => ({
    entries: [],
    nextSequence: afterSequence,
  }),
  onOpenBackgroundAgent,
  onOpenOutput,
  busy,
  animateStreaming = true,
  compacting = false,
  streamStalled = false,
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
  hydrated = true,
  imageClient,
  executionConfigClient,
  changeClient,
  backgroundAgentClient,
}: MessageListProps) {
  // Stable identity between renders so memoized rows only re-render when the
  // approval state itself changes, not on every streamed token.
  const approvalState = useMemo(
    () => ({ decidingApprovalCalls, approvalErrors, grantScope }),
    [decidingApprovalCalls, approvalErrors, grantScope],
  );
  const retry = useMemo(() => {
    if (!onRetryTurn) return undefined;
    const turn = retryableTurn(messages);
    if (!turn) return undefined;
    return { failureId: turn.failureId, onRetry: () => onRetryTurn(turn) };
  }, [messages, onRetryTurn]);
  const { items: messageItems, lastTurnStart } = groupMessageItems(
    messages,
    busy,
    animateStreaming,
    onApproval,
    approvalState,
    imageClient,
    chatId,
    changeClient,
    {
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
    },
    retry,
  );
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
      <div className="messages is-empty" ref={scrollRef} onScroll={onScroll}>
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
      <div className="messages" ref={scrollRef} onScroll={onScroll}>
        <div className="messages-column">
          <TranscriptSkeleton />
        </div>
      </div>
    );
  }

  // The continuation cards and the working indicator belong to the turn in
  // flight, so when the trailing turn is pinned they ride inside its wrapper.
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
      ) : (
        shouldShowAssistantWorking(
          messages,
          busy,
          folderAccessRequests.length +
            outputWritebackRequests.length +
            pendingPromptCount,
          streamStalled,
        ) && <AssistantWorkingIndicator />
      )}
    </>
  );

  const pin = pinLastTurn && lastTurnStart >= 0;

  return (
    <div className="messages" ref={scrollRef} onScroll={onScroll}>
      <div className="messages-column" ref={contentRef}>
        {pin ? (
          <>
            {messageItems.slice(0, lastTurnStart)}
            <div className="message-turn is-pinned">
              {messageItems.slice(lastTurnStart)}
              {trailing}
            </div>
          </>
        ) : (
          <>
            {messageItems}
            {trailing}
          </>
        )}
      </div>
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
  backgroundAgents: {
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
  } = {
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
  retry?: { failureId: string; onRetry: () => void },
) {
  const items: ReactNode[] = [];
  // The item index at which the trailing turn opens (its user message). Lets the
  // caller lift the last exchange into a pinned wrapper without re-deriving the
  // turn boundary. Stays -1 for a transcript that opens on activity alone.
  let lastTurnStart = -1;
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

  while (index < messages.length) {
    const message = messages[index];

    if (!isActivityMessage(message)) {
      if (message.role === "user") {
        lastTurnStart = items.length;
        standingCardKeys = new Set<string>();
        turnWebSources = [];
      }
      const closesTurn =
        message.role === "assistant" && isTurnClosingAssistant(messages, index);
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
          onRetry={retry?.failureId === message.id ? retry.onRetry : undefined}
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
    const phase: ChatMessage[] = [];
    // Activity can resume through several assistant snapshots while remaining
    // one expandable phase. The collapsed live label should describe only the
    // latest snapshot; the expanded rail still preserves the whole phase.
    let latestSnapshotStart = 0;
    while (index < messages.length) {
      if (isActivityMessage(messages[index])) {
        phase.push(messages[index]!);
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
          latestSnapshotStart = phase.length;
          index = ahead;
          continue;
        }
      }
      break;
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
    // Phases accumulate: a turn that searched, answered a little, and searched
    // again names every page it found under the answer that closes it.
    for (const source of collectWebSources(activities)) {
      if (!turnWebSources.some((seen) => seen.url === source.url)) {
        turnWebSources.push(source);
      }
    }
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

    // The rail and every card inside carry their own boundary, so this one is
    // only a backstop for the phase's own frame.
    items.push(
      <ErrorBoundary
        key={`tool-activity-group-${groupIndex}`}
        fallback={<ToolActivityUnavailable />}
      >
        <ToolActivityGroup
          activities={railActivities}
          labelActivities={latestRailActivities}
          anchorIds={phase.flatMap((entry) =>
            entry.role === "tool" ? [entry.id] : [],
          )}
          groupIndex={groupIndex}
          animate={animateStreaming}
        >
          {children.length > 0 ? children : undefined}
        </ToolActivityGroup>
      </ErrorBoundary>,
    );
    groupIndex += 1;
  }

  return { items, lastTurnStart };
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
  onRetry,
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
  /** Present only on the transcript's newest retryable failure. */
  onRetry?: () => void;
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
          <MessageMarkdown>{message.text}</MessageMarkdown>
        </article>
      );
    }

    return (
      <MessageCitationsProvider value={citations}>
        <article className="message message-assistant" aria-label="Assistant">
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
          />
        </article>
      </MessageCitationsProvider>
    );
  }

  if (message.role === "user") {
    return (
      <UserMessage
        text={message.text}
        createdAt={message.createdAt}
        anchorId={message.id}
        leading={
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
        }
        trailing={
          message.invokedSkills && message.invokedSkills.length > 0 ? (
            <TranscriptInvokedSkills skills={message.invokedSkills} />
          ) : null
        }
      />
    );
  }

  if (message.role === "system" || message.role === "error") {
    return (
      <div
        className={`message-notice is-${message.role}`}
        role={message.role === "error" ? "alert" : "status"}
      >
        {message.text}
      </div>
    );
  }

  if (message.role === "compaction") {
    return (
      <div className="message-notice is-compaction" role="status">
        Compacted conversation
      </div>
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

  if (message.role === "refusal") {
    return (
      <div className="message-notice is-refusal" role="status">
        {refusalCopy(message.category, message.partialOutput)}
      </div>
    );
  }

  return null;
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
): string {
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
