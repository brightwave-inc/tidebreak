import { memo, useMemo } from "react";
import type { ReactNode, RefObject, UIEvent } from "react";
import type {
  ApprovalGrantRung,
  PendingFolderAccessRequest,
  PendingUserQuestions,
  ToolActionPreview,
  ToolResultPreview,
  UserQuestionAnswer,
} from "./api";
import { ApprovalCard } from "./ApprovalCard";
import { AssistantWorkingIndicator } from "./AssistantWorkingIndicator";
import { FolderAccessCard } from "./FolderAccessCard";
import type { FolderAccessDecision } from "./host";
import { MessageMarkdown } from "./MessageMarkdown";
import { MessageFooter } from "./MessageFooter";
import { AssistantSources, type AssistantSource } from "./AssistantSources";
import { McpAppCard } from "./McpAppCard";
import { ToolCommandCard, type ToolCallStatus } from "./ToolCallCard";
import { ErrorBoundary } from "./ErrorBoundary";
import { ToolActivityGroup } from "./ToolActivityGroup";
import { WelcomeState } from "./WelcomeState";
import { UserQuestionsCard } from "./UserQuestionsCard";

export type ChatMessage =
  | { id: string; role: "user"; text: string; createdAt?: string }
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
      /** The tool's own closed view of what it is doing, when it has one. */
      preview?: ToolActionPreview | null;
      /** What the call produced, once it has produced anything. */
      result?: ToolResultPreview | null;
    }
  | {
      id: string;
      role: "approval";
      callId: string;
      summary: string;
      preview?: ToolActionPreview | null;
      canApprove: boolean;
      canRemember: boolean;
      resolved?: boolean;
    }
  | { id: string; role: "error"; text: string };

type MessageListProps = {
  messages: ChatMessage[];
  folderAccessRequests: PendingFolderAccessRequest[];
  userQuestionRequests?: PendingUserQuestions[];
  nativeHost: boolean;
  nativeBusy: boolean;
  resolvingFolderCalls: Set<string>;
  folderAccessErrors: Record<string, string>;
  answeringQuestionCalls?: Set<string>;
  userQuestionErrors?: Record<string, string>;
  decidingApprovalCalls: Set<string>;
  approvalErrors: Record<string, string>;
  busy: boolean;
  reasoningActive?: boolean;
  scrollRef: RefObject<HTMLDivElement | null>;
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
  onAnswerUserQuestions?: (
    callId: string,
    answers: UserQuestionAnswer[],
  ) => void;
  onUserQuestionsCancel?: (turnId: string) => void;
  onSelectPrompt?: (prompt: string) => void;
  hydrated?: boolean;
};

export function MessageList({
  messages,
  folderAccessRequests,
  userQuestionRequests = [],
  nativeHost,
  nativeBusy,
  resolvingFolderCalls,
  folderAccessErrors,
  answeringQuestionCalls = new Set(),
  userQuestionErrors = {},
  decidingApprovalCalls,
  approvalErrors,
  busy,
  reasoningActive = false,
  scrollRef,
  onScroll,
  onApproval,
  onFolderAccessDecision,
  onFolderAccessCancel,
  onAnswerUserQuestions = () => undefined,
  onUserQuestionsCancel = () => undefined,
  onSelectPrompt,
  hydrated = true,
}: MessageListProps) {
  // Stable identity between renders so memoized rows only re-render when the
  // approval state itself changes, not on every streamed token.
  const approvalState = useMemo(
    () => ({ decidingApprovalCalls, approvalErrors }),
    [decidingApprovalCalls, approvalErrors],
  );
  const messageItems = groupMessageItems(
    messages,
    busy,
    onApproval,
    approvalState,
  );
  // Only greet a genuinely empty, fully-hydrated conversation. While an
  // existing chat's transcript is still loading it is transiently empty; showing
  // the welcome there would flash "How can I help?" before its history renders.
  const isEmpty =
    hydrated &&
    messages.length === 0 &&
    folderAccessRequests.length === 0 &&
    userQuestionRequests.length === 0 &&
    !busy;

  if (isEmpty) {
    return (
      <div className="messages is-empty" ref={scrollRef} onScroll={onScroll}>
        <WelcomeState onSelectPrompt={onSelectPrompt} />
      </div>
    );
  }

  return (
    <div className="messages" ref={scrollRef} onScroll={onScroll}>
      <div className="messages-column">
        {messageItems}
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
        {userQuestionRequests.map((request) => (
          <UserQuestionsCard
            key={request.callId}
            request={request}
            working={answeringQuestionCalls.has(request.callId)}
            error={userQuestionErrors[request.callId]}
            onAnswer={(answers) =>
              onAnswerUserQuestions(request.callId, answers)
            }
            onCancel={() => onUserQuestionsCancel(request.turnId)}
          />
        ))}
        {shouldShowAssistantWorking(
          messages,
          busy,
          folderAccessRequests.length + userQuestionRequests.length,
        ) && <AssistantWorkingIndicator thinking={reasoningActive} />}
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
  },
) {
  const items: ReactNode[] = [];
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
      items.push(
        <MessageBubble
          key={message.id}
          message={message}
          busy={message.id === streamingAssistantId}
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
    const cards = surfacedCards(phase, parked, onApproval, approvalState);

    // A tool row renders model-influenced data through several defensive
    // parsers. If one of them is ever wrong, the throw should cost this phase
    // and not the conversation around it.
    items.push(
      <ErrorBoundary
        key={`tool-activity-group-${groupIndex}`}
        fallback={
          <p className="tool-activity-unavailable" role="status">
            This step could not be displayed.
          </p>
        }
      >
        <ToolActivityGroup activities={activities} groupIndex={groupIndex}>
          {cards.length > 0 ? cards : undefined}
        </ToolActivityGroup>
      </ErrorBoundary>,
    );
    groupIndex += 1;
  }

  return items;
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
  },
): ReactNode[] {
  const cards: ReactNode[] = [];
  // In the order the calls happened, so the cards read as a sequence rather
  // than as two piles sorted by what kind of card they are.
  for (const entry of phase) {
    if (entry.role === "approval") {
      if (entry.resolved) continue;
      cards.push(
        <ApprovalCard
          key={entry.id}
          callId={entry.callId}
          summary={entry.summary}
          preview={entry.preview ?? null}
          canApprove={entry.canApprove}
          canRemember={entry.canRemember}
          deciding={
            approvalState?.decidingApprovalCalls.has(entry.callId) ?? false
          }
          error={approvalState?.approvalErrors[entry.callId]}
          onDecide={onApproval}
        />,
      );
      continue;
    }
    if (entry.role !== "tool") continue;
    // An MCP App view is keyed on the *result*: the tool has no action
    // preview, and its card exists to show what the server's declared view
    // renders — inside the sandbox — not to restate arguments or output.
    if (entry.result?.tool === "mcp_app" && !parked.has(entry.callId)) {
      cards.push(
        <McpAppCard
          key={entry.id}
          server={entry.result.server}
          resourceUri={entry.result.resourceUri}
        />,
      );
      continue;
    }
    // The approval card already shows this command and owns the decision.
    if (!entry.preview || parked.has(entry.callId)) continue;
    // A card earns its place by carrying something the rail cannot: a command
    // and its output. A search's query is fully said by the rail line and by
    // the approval card that asked about it, so it does not get an
    // exec-shaped card with tabs and an exit code.
    if (entry.preview.tool !== "exec") continue;
    cards.push(
      <ToolCommandCard
        key={entry.id}
        name={entry.name}
        status={entry.status}
        preview={entry.preview}
        result={entry.result?.tool === "exec" ? entry.result : null}
      />,
    );
  }
  return cards;
}

/**
 * Memoized row: settled messages keep referential identity across reducer
 * transitions, so during streaming only the live assistant bubble (whose
 * message object changes each token) re-renders.
 */
export const MessageBubble = memo(MessageBubbleImpl);

/**
 * One conversational turn. Tool calls and approvals are not turns — they belong
 * to an activity phase, which owns both the rail and the cards below it.
 */
function MessageBubbleImpl({
  message,
  busy,
}: {
  message: ChatMessage;
  busy: boolean;
}) {
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
      <article className="message message-assistant" aria-label="Assistant">
        {message.text && <MessageMarkdown>{message.text}</MessageMarkdown>}
        <AssistantSources sources={message.sources} />
        <MessageFooter
          role="assistant"
          text={message.text}
          createdAt={message.createdAt}
          settled={!busy}
        />
      </article>
    );
  }

  if (message.role === "user") {
    return (
      <div className="message-user-frame">
        <article className="message message-user" aria-label="You">
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

  if (message.role === "refusal") {
    return (
      <div className="message-notice is-refusal" role="status">
        {refusalCopy(message.category, message.partialOutput)}
      </div>
    );
  }

  return null;
}

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
  if (latest?.role === "system" || latest?.role === "error") return false;
  return true;
}
