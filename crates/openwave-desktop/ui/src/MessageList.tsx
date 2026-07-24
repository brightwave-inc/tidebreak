import { memo, useMemo } from "react";
import type { ReactNode, RefObject, UIEvent } from "react";
import type {
  PendingFolderAccessRequest,
  PendingUserQuestions,
  ToolApprovalPreview,
  UserQuestionAnswer,
} from "./api";
import { ApprovalCard } from "./ApprovalCard";
import { AssistantWorkingIndicator } from "./AssistantWorkingIndicator";
import { FolderAccessCard } from "./FolderAccessCard";
import type { FolderAccessDecision } from "./host";
import { MessageMarkdown } from "./MessageMarkdown";
import { MessageFooter } from "./MessageFooter";
import { AssistantSources, type AssistantSource } from "./AssistantSources";
import { ToolCallCard, type ToolCallStatus } from "./ToolCallCard";
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
      role: "tool";
      callId: string;
      name: string;
      status: ToolCallStatus;
      /** The tool's own closed view of what it is doing, when it has one. */
      preview?: ToolApprovalPreview | null;
    }
  | {
      id: string;
      role: "approval";
      callId: string;
      summary: string;
      preview?: ToolApprovalPreview | null;
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
    remember?: boolean,
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

function isGroupableTerminalTool(
  message: ChatMessage,
): message is Extract<ChatMessage, { role: "tool" }> {
  // Folder access is an authority boundary. It stays separate even when its
  // corresponding tool event is terminal, so it cannot be mistaken for a
  // passive historical activity item.
  return (
    message.role === "tool" &&
    message.name !== "request_folder_access" &&
    (message.status === "completed" ||
      message.status === "failed" ||
      message.status === "cancelled")
  );
}

export function groupMessageItems(
  messages: ChatMessage[],
  busy: boolean,
  onApproval: (
    callId: string,
    decision: "approve" | "reject",
    remember?: boolean,
  ) => void,
  approvalState?: {
    decidingApprovalCalls: Set<string>;
    approvalErrors: Record<string, string>;
  },
) {
  // While a call is parked, its approval card already names the action and
  // carries the controls. Showing the tool card beside it says the same thing
  // twice and puts a second, inert copy of the command on screen.
  const parkedOnApproval = new Set(
    messages.flatMap((message) =>
      message.role === "approval" && !message.resolved ? [message.callId] : [],
    ),
  );
  const visible = messages.filter(
    (message) =>
      !(
        message.role === "tool" &&
        message.status === "waiting_approval" &&
        parkedOnApproval.has(message.callId)
      ),
  );

  const items: ReactNode[] = [];
  let index = 0;
  let groupIndex = 0;
  let streamingAssistantId: string | undefined;
  if (busy) {
    for (
      let messageIndex = visible.length - 1;
      messageIndex >= 0;
      messageIndex -= 1
    ) {
      const candidate = visible[messageIndex];
      if (candidate?.role === "assistant" && !candidate.superseded) {
        streamingAssistantId = candidate.id;
        break;
      }
      // A newly submitted user message is busy before its turn-start event
      // arrives. Do not make the preceding completed assistant look live.
      if (candidate?.role === "user") break;
    }
  }

  while (index < visible.length) {
    const message = visible[index];

    if (!isGroupableTerminalTool(message)) {
      items.push(
        <MessageBubble
          key={message.id}
          message={message}
          busy={message.id === streamingAssistantId}
          onApproval={onApproval}
          approvalState={approvalState}
        />,
      );
      index += 1;
      continue;
    }

    const activities = [message];
    index += 1;
    while (index < visible.length) {
      const nextMessage = visible[index];
      if (!isGroupableTerminalTool(nextMessage)) {
        break;
      }

      activities.push(nextMessage);
      index += 1;
    }

    if (activities.length === 1) {
      items.push(
        <MessageBubble
          key={message.id}
          message={message}
          busy={message.id === streamingAssistantId}
          onApproval={onApproval}
          approvalState={approvalState}
        />,
      );
      continue;
    }

    items.push(
      <ToolActivityGroup
        key={`tool-activity-group-${groupIndex}`}
        activities={activities}
        groupIndex={groupIndex}
      />,
    );
    groupIndex += 1;
  }

  return items;
}

/**
 * Memoized row: settled messages keep referential identity across reducer
 * transitions, so during streaming only the live assistant bubble (whose
 * message object changes each token) re-renders.
 */
export const MessageBubble = memo(MessageBubbleImpl);

function MessageBubbleImpl({
  message,
  busy,
  onApproval,
  approvalState,
}: {
  message: ChatMessage;
  busy: boolean;
  onApproval: (
    callId: string,
    decision: "approve" | "reject",
    remember?: boolean,
  ) => void;
  approvalState?: {
    decidingApprovalCalls: Set<string>;
    approvalErrors: Record<string, string>;
  };
}) {
  if (message.role === "tool") {
    return (
      <ToolCallCard
        name={message.name}
        status={message.status}
        preview={message.preview}
      />
    );
  }

  if (message.role === "approval") {
    const deciding =
      approvalState?.decidingApprovalCalls.has(message.callId) ?? false;
    // A decided card keeps its question so the transcript still records what
    // was asked, but drops the controls that would imply it is still open.
    if (message.resolved) {
      return (
        <section
          className="bg-card text-muted-foreground w-[min(100%,38rem)] self-start rounded-lg border p-4 text-sm"
          aria-label="Approval needed"
        >
          {message.summary}
        </section>
      );
    }
    return (
      <ApprovalCard
        callId={message.callId}
        summary={message.summary}
        preview={message.preview ?? null}
        canApprove={message.canApprove}
        canRemember={message.canRemember}
        deciding={deciding}
        error={approvalState?.approvalErrors[message.callId]}
        onDecide={onApproval}
      />
    );
  }

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

  return (
    <div
      className={`message-notice is-${message.role}`}
      role={message.role === "error" ? "alert" : "status"}
    >
      {message.text}
    </div>
  );
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
