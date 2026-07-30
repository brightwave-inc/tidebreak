import { Fragment, memo, useMemo } from "react";
import type { ReactNode, Ref, RefCallback, UIEvent } from "react";
import type {
  ApprovalGrantRung,
  ApiClient,
  AgentRun,
  AgentActivityHistoryEntry,
  PendingFolderAccessRequest,
  PendingOutputWritebackRequest,
  PendingUserQuestions,
  ToolActionPreview,
  ToolResultPreview,
  UserQuestionAnswer,
} from "./api";
import { ApprovalCard, type GrantScopeName } from "./ApprovalCard";
import { AssistantWorkingIndicator } from "./AssistantWorkingIndicator";
import { FolderAccessCard } from "./FolderAccessCard";
import type {
  FolderAccessDecision,
  OutputWritebackDecision,
} from "./host";
import { OutputWritebackCard } from "./OutputWritebackCard";
import { MessageMarkdown } from "./MessageMarkdown";
import { MessageFooter } from "./MessageFooter";
import { AssistantSources, type AssistantSource } from "./AssistantSources";
import { stripCitationDirectives } from "./citationDirectives";
import { MessageCitationsProvider } from "./InlineCitation";
import { McpAppCard } from "./McpAppCard";
import { ToolCommandCard, type ToolCallStatus } from "./ToolCallCard";
import { ErrorBoundary } from "./ErrorBoundary";
import {
  ToolActivityGroup,
  ToolActivityUnavailable,
} from "./ToolActivityGroup";
import { WelcomeState } from "./WelcomeState";
import { UserQuestionsCard } from "./UserQuestionsCard";
import type { TranscriptImageAttachment } from "./ImageAttachments";
import { TranscriptImageAttachments } from "./TranscriptImageAttachments";
import {
  TranscriptFileAttachments,
  type TranscriptFileAttachment,
} from "./TranscriptFileAttachments";
import { BackgroundAgentList } from "./BackgroundAgentList";
import { WebSearchProviderRequiredCard } from "./WebSearchProviderRequiredCard";
import { useSourceNav } from "./panel/SourceNav";
import {
  TurnFailureNotice,
  turnFailureOffersRetry,
} from "./TurnFailureNotice";
import type { TurnFailureCategory } from "./generated/wire";
import { useStreamingTypewriter } from "./useStreamingTypewriter";
import { Skeleton } from "./components/ui/skeleton";

export type ChatMessage =
  | {
      id: string;
      role: "user";
      text: string;
      images?: TranscriptImageAttachment[];
      files?: TranscriptFileAttachment[];
      createdAt?: string;
    }
  | {
      id: string;
      role: "assistant";
      text: string;
      sources: AssistantSource[];
      createdAt?: string;
      /** Interrupted mid-stream and replaced; rendered dimmed until the
       *  authoritative transcript sweeps it. */
      superseded?: boolean;
    }
  | { id: string; role: "system"; text: string }
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
      /** Prefix rungs the server will honor for this call. */
      prefixRungs?: readonly number[];
      resolved?: boolean;
    }
  | { id: string; role: "error"; text: string }
  | { id: string; role: "turn_failure"; category: TurnFailureCategory };

/** Everything a retry needs to put the failed turn back on the wire unchanged. */
export type RetryableTurn = {
  /** The failure notice that offers this retry. */
  failureId: string;
  text: string;
  images: readonly TranscriptImageAttachment[];
  files: readonly TranscriptFileAttachment[];
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
  userQuestionRequests?: PendingUserQuestions[];
  nativeHost: boolean;
  nativeBusy: boolean;
  resolvingFolderCalls: Set<string>;
  folderAccessErrors: Record<string, string>;
  resolvingOutputWritebackCalls?: Set<string>;
  outputWritebackErrors?: Record<string, string>;
  answeringQuestionCalls?: Set<string>;
  userQuestionErrors?: Record<string, string>;
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
  onViewBackgroundAgentOutput?: () => void;
  busy: boolean;
  reasoningActive?: boolean;
  scrollRef: Ref<HTMLDivElement>;
  /** Attached to the transcript content column so growth can drive auto-follow. */
  contentRef?: RefCallback<HTMLDivElement>;
  /** Extra classes for the scroll container — used for the scroll-edge fades. */
  maskClass?: string | null;
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
  onAnswerUserQuestions?: (
    callId: string,
    answers: UserQuestionAnswer[],
  ) => void;
  onUserQuestionsCancel?: (turnId: string) => void;
  onSelectPrompt?: (prompt: string) => void;
  /** Resend the failed turn. Offered only on the transcript's newest failure. */
  onRetryTurn?: (turn: RetryableTurn) => void;
  hydrated?: boolean;
  imageClient?: Pick<ApiClient, "getChatImageAttachment">;
};

export function MessageList({
  messages,
  chatId,
  folderAccessRequests,
  outputWritebackRequests = [],
  userQuestionRequests = [],
  nativeHost,
  nativeBusy,
  resolvingFolderCalls,
  folderAccessErrors,
  resolvingOutputWritebackCalls = new Set(),
  outputWritebackErrors = {},
  answeringQuestionCalls = new Set(),
  userQuestionErrors = {},
  decidingApprovalCalls,
  approvalErrors,
  grantScope,
  backgroundAgentRuns = [],
  backgroundAgentRunsLoading = false,
  backgroundAgentRunsError = null,
  onRetryBackgroundAgentRuns = () => undefined,
  onCancelBackgroundAgentRun = async () => undefined,
  onLoadBackgroundAgentActivity = async () => [],
  onViewBackgroundAgentOutput,
  busy,
  reasoningActive = false,
  scrollRef,
  contentRef,
  maskClass,
  pinLastTurn = false,
  onScroll,
  onApproval,
  onFolderAccessDecision,
  onFolderAccessCancel,
  onOutputWritebackDecision = () => undefined,
  onOutputWritebackCancel = () => undefined,
  onAnswerUserQuestions = () => undefined,
  onUserQuestionsCancel = () => undefined,
  onSelectPrompt,
  onRetryTurn,
  hydrated = true,
  imageClient,
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
    onApproval,
    approvalState,
    imageClient,
    chatId,
    {
      runs: backgroundAgentRuns,
      loading: backgroundAgentRunsLoading,
      error: backgroundAgentRunsError,
      retry: onRetryBackgroundAgentRuns,
      cancel: onCancelBackgroundAgentRun,
      loadActivity: onLoadBackgroundAgentActivity,
      viewOutput: onViewBackgroundAgentOutput,
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
    userQuestionRequests.length === 0 &&
    !busy;

  if (isEmpty) {
    return (
      <div className="messages is-empty" ref={scrollRef} onScroll={onScroll}>
        <WelcomeState onSelectPrompt={onSelectPrompt} />
      </div>
    );
  }

  // A conversation that hasn't hydrated yet is transiently empty; a skeleton
  // holds the shape of a transcript so the pane doesn't flash blank before the
  // history lands.
  if (!hydrated && messages.length === 0) {
    return (
      <div
        className={`messages${maskClass ? ` ${maskClass}` : ""}`}
        ref={scrollRef}
        onScroll={onScroll}
      >
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
      {folderAccessRequests.map((request) => (
        <FolderAccessCard
          key={request.callId}
          request={request}
          nativeHost={nativeHost}
          nativeBusy={nativeBusy}
          working={resolvingFolderCalls.has(request.callId)}
          error={folderAccessErrors[request.callId]}
          onDecision={(decision) =>
            onFolderAccessDecision(request.callId, decision)
          }
          onCancel={() => onFolderAccessCancel(request.callId, request.turnId)}
        />
      ))}
      {outputWritebackRequests.map((request) => (
        <OutputWritebackCard
          key={request.callId}
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
        />
      ))}
      {userQuestionRequests.map((request) => (
        <UserQuestionsCard
          key={request.callId}
          request={request}
          working={answeringQuestionCalls.has(request.callId)}
          error={userQuestionErrors[request.callId]}
          onAnswer={(answers) => onAnswerUserQuestions(request.callId, answers)}
          onCancel={() => onUserQuestionsCancel(request.turnId)}
        />
      ))}
      {shouldShowAssistantWorking(
        messages,
        busy,
        folderAccessRequests.length +
          outputWritebackRequests.length +
          userQuestionRequests.length,
      ) && <AssistantWorkingIndicator thinking={reasoningActive} />}
    </>
  );

  const pin = pinLastTurn && lastTurnStart >= 0;

  return (
    <div
      className={`messages${maskClass ? ` ${maskClass}` : ""}`}
      ref={scrollRef}
      onScroll={onScroll}
    >
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

/** Placeholder rows that echo the transcript's shape while history hydrates. */
function TranscriptSkeleton() {
  return (
    <div className="flex w-full flex-col gap-4" aria-hidden="true">
      {[0, 1, 2, 3].map((row) => (
        <Fragment key={row}>
          <Skeleton className="h-9 w-1/2 self-start rounded-xl" />
          <div className="flex flex-col gap-2.5 py-2">
            <Skeleton className="h-3 w-full" />
            <Skeleton className="h-3 w-full" />
            <Skeleton className="h-3 w-full" />
            <Skeleton className="h-3 w-5/6" />
            <Skeleton className="h-3 w-1/3" />
          </div>
        </Fragment>
      ))}
    </div>
  );
}

type ToolMessage = Extract<ChatMessage, { role: "tool" }>;

/** Whether this message belongs to an activity phase rather than to the conversation. */
function isActivityMessage(message: ChatMessage | undefined): boolean {
  if (message === undefined) return false;
  return message.role === "tool" || message.role === "approval";
}

export function groupMessageItems(
  messages: ChatMessage[],
  busy: boolean,
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
  backgroundAgents: {
    runs: AgentRun[];
    loading: boolean;
    error: string | null;
    retry: () => void;
    cancel: (runId: string) => Promise<void>;
    loadActivity: (runId: string) => Promise<AgentActivityHistoryEntry[]>;
    viewOutput?: () => void;
  } = {
    runs: [],
    loading: false,
    error: null,
    retry: () => undefined,
    cancel: async () => undefined,
    loadActivity: async () => [],
  },
  retry?: { failureId: string; onRetry: () => void },
) {
  const items: ReactNode[] = [];
  // The item index at which the trailing turn opens (its user message). Lets the
  // caller lift the last exchange into a pinned wrapper without re-deriving the
  // turn boundary. Stays -1 for a transcript that opens on activity alone.
  let lastTurnStart = -1;
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
      if (message.role === "user") lastTurnStart = items.length;
      items.push(
        <MessageBubble
          key={message.id}
          message={message}
          busy={message.id === streamingAssistantId}
          imageClient={imageClient}
          chatId={chatId}
          onRetry={
            retry?.failureId === message.id ? retry.onRetry : undefined
          }
        />,
      );
      index += 1;
      continue;
    }

    // One phase per contiguous run of activity, however long. A phase that
    // splits at every assistant sentence is not a phase.
    const phase: ChatMessage[] = [];
    while (index < messages.length && isActivityMessage(messages[index])) {
      phase.push(messages[index]!);
      index += 1;
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
    const cards = surfacedCards(
      phase,
      parked,
      onApproval,
      approvalState,
      chatId,
      imageClient,
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
            onViewOutput={backgroundAgents.viewOutput}
          />,
        ),
      );
    }

    // The rail and every card inside carry their own boundary, so this one is
    // only a backstop for the phase's own frame.
    items.push(
      <ErrorBoundary
        key={`tool-activity-group-${groupIndex}`}
        fallback={<ToolActivityUnavailable />}
      >
        <ToolActivityGroup activities={activities} groupIndex={groupIndex}>
          {children.length > 0 ? children : undefined}
        </ToolActivityGroup>
      </ErrorBoundary>,
    );
    groupIndex += 1;
  }

  return { items, lastTurnStart };
}

/**
 * One card, contained.
 *
 * Cards render model-influenced data through several defensive parsers, and one
 * of them being wrong must not cost the card next to it. The sibling that makes
 * this matter is the approval prompt: a phase parked on a decision that renders
 * as nothing leaves the reader with no way to answer and no explanation.
 *
 * `signature` is the data the card draws on, reduced to what decides whether it
 * can render, so a card that threw mid-stream is retried when its call moves on
 * rather than staying broken for the life of the transcript.
 */
function isolatedCard(
  key: string,
  signature: string,
  card: ReactNode,
): ReactNode {
  return (
    <ErrorBoundary
      key={key}
      resetKey={signature}
      fallback={<ToolActivityUnavailable />}
    >
      {card}
    </ErrorBoundary>
  );
}

/**
 * The cards that hang below a phase, always visible.
 *
 * A call earns one by having something a line of text can't carry: a command to
 * read, or a decision to make. Anything a card would show that the rail already
 * says stays in the rail.
 */
function surfacedCards(
  phase: ChatMessage[],
  parked: Set<string>,
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
  imageClient?: Pick<ApiClient, "getChatImageAttachment">,
): ReactNode[] {
  const cards: ReactNode[] = [];
  // In the order the calls happened, so the cards read as a sequence rather
  // than as two piles sorted by what kind of card they are.
  for (const entry of phase) {
    if (entry.role === "approval") {
      if (entry.resolved) continue;
      cards.push(
        isolatedCard(
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
            prefixRungs={entry.prefixRungs ?? []}
            deciding={
              approvalState?.decidingApprovalCalls.has(entry.callId) ?? false
            }
            error={approvalState?.approvalErrors[entry.callId]}
            onDecide={onApproval}
          />,
        ),
      );
      continue;
    }
    if (entry.role !== "tool") continue;
    if (
      entry.result?.tool === "web_search_provider_required" &&
      !parked.has(entry.callId)
    ) {
      cards.push(isolatedCard(entry.id, "", <WebSearchProviderRequiredCard />));
      continue;
    }
    // An MCP App view is keyed on the *result*: the tool has no action
    // preview, and its card exists to show what the server's declared view
    // renders — inside the sandbox — not to restate arguments or output.
    if (entry.result?.tool === "mcp_app" && !parked.has(entry.callId)) {
      cards.push(
        isolatedCard(
          entry.id,
          `${entry.result.server} ${entry.result.resourceUri}`,
          <McpAppCard
            server={entry.result.server}
            resourceUri={entry.result.resourceUri}
            chatId={chatId}
            callId={entry.callId}
          />,
        ),
      );
      continue;
    }
    // What a call found, read, or wrote renders inside the expanded rail,
    // under its own row — collapsed, a phase is one line, and a run of
    // searches must not stack a column of standing cards.
    // The approval card already shows this command and owns the decision.
    if (!entry.preview || parked.has(entry.callId)) continue;
    // A card earns its place by carrying something the rail cannot: a command
    // and its output. A search's query is fully said by the rail line and by
    // the approval card that asked about it, so it does not get an
    // exec-shaped card with tabs and an exit code.
    if (entry.preview.tool !== "exec") continue;
    cards.push(
      isolatedCard(
        entry.id,
        `${entry.status} ${entry.result?.tool ?? ""}`,
        <ToolCommandCard
          name={entry.name}
          status={entry.status}
          preview={entry.preview}
          result={entry.result?.tool === "exec" ? entry.result : null}
          imageClient={imageClient}
          chatId={chatId}
        />,
      ),
    );
  }
  return cards;
}

/**
 * Assistant prose driven by the typewriter: while the bubble is the live
 * streaming turn its text is typed in, and a settled or rehydrated message
 * renders at once. Block-level memoization inside {@link MessageMarkdown} keeps
 * each tick's re-parse confined to the trailing block.
 */
function AssistantMessageBody({
  text,
  streaming,
}: {
  text: string;
  streaming: boolean;
}) {
  const displayed = useStreamingTypewriter(text, streaming);
  return <MessageMarkdown>{displayed}</MessageMarkdown>;
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
  imageClient,
  chatId,
  onRetry,
}: {
  message: ChatMessage;
  busy: boolean;
  imageClient?: Pick<ApiClient, "getChatImageAttachment">;
  chatId?: string;
  /** Present only on the transcript's newest retryable failure. */
  onRetry?: () => void;
}) {
  const sourceNav = useSourceNav();
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
  const sources = message.role === "assistant" ? message.sources : EMPTY_SOURCES;
  const citations = useMemo(
    () => ({ sources, onOpenSource: openSource }),
    [sources, openSource],
  );

  if (message.role === "assistant") {
    if (!message.text && message.sources.length === 0) return null;

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
          {message.text && (
            <AssistantMessageBody text={message.text} streaming={busy} />
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
          />
        </article>
      </MessageCitationsProvider>
    );
  }

  if (message.role === "user") {
    return (
      <div className="message-user-frame">
        <article className="message message-user" aria-label="You">
          {message.images && message.images.length > 0 && imageClient && chatId && (
            <TranscriptImageAttachments
              client={imageClient}
              chatId={chatId}
              images={message.images}
            />
          )}
          {message.files && message.files.length > 0 && (
            <TranscriptFileAttachments files={message.files} />
          )}
          <MessageMarkdown>{message.text}</MessageMarkdown>
        </article>
        <MessageFooter
          role="user"
          text={message.text}
          createdAt={message.createdAt}
        />
      </div>
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

  if (message.role === "turn_failure") {
    return <TurnFailureNotice category={message.category} onRetry={onRetry} />;
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
 */
export function shouldShowAssistantWorking(
  messages: readonly ChatMessage[],
  busy: boolean,
  pendingFolderAccessCount: number,
): boolean {
  if (!busy || pendingFolderAccessCount > 0) return false;
  const hasSpecificPendingStatus = messages.some(
    (message) =>
      (message.role === "tool" &&
        message.status !== "completed" &&
        message.status !== "failed" &&
        message.status !== "cancelled") ||
      (message.role === "approval" && !message.resolved),
  );
  if (hasSpecificPendingStatus) return false;

  const latest = messages[messages.length - 1];
  if (latest?.role === "assistant") {
    return latest.text.trim().length === 0 && latest.sources.length === 0;
  }
  if (
    latest?.role === "system" ||
    latest?.role === "error" ||
    latest?.role === "turn_failure"
  ) {
    return false;
  }
  return true;
}
