import type { RefObject, UIEvent } from "react";
import type { PendingFolderAccessRequest } from "./api";
import { FolderAccessCard } from "./FolderAccessCard";
import type { FolderAccessDecision } from "./host";
import { MessageMarkdown } from "./MessageMarkdown";
import { ToolCallCard, type ToolCallStatus } from "./ToolCallCard";

export type ChatMessage =
  | { id: string; role: "user"; text: string }
  | { id: string; role: "assistant"; text: string }
  | { id: string; role: "system"; text: string }
  | {
      id: string;
      role: "tool";
      callId: string;
      name: string;
      status: ToolCallStatus;
    }
  | {
      id: string;
      role: "approval";
      callId: string;
      summary: string;
      resolved?: boolean;
    }
  | { id: string; role: "error"; text: string };

type MessageListProps = {
  messages: ChatMessage[];
  folderAccessRequests: PendingFolderAccessRequest[];
  nativeHost: boolean;
  nativeBusy: boolean;
  resolvingFolderCalls: Set<string>;
  folderAccessErrors: Record<string, string>;
  busy: boolean;
  scrollRef: RefObject<HTMLDivElement | null>;
  onScroll: (event: UIEvent<HTMLDivElement>) => void;
  onApproval: (callId: string, decision: "approve" | "reject") => void;
  onFolderAccessDecision: (
    callId: string,
    decision: FolderAccessDecision,
  ) => void;
  onFolderAccessCancel: (callId: string, turnId: string) => void;
};

export function MessageList({
  messages,
  folderAccessRequests,
  nativeHost,
  nativeBusy,
  resolvingFolderCalls,
  folderAccessErrors,
  busy,
  scrollRef,
  onScroll,
  onApproval,
  onFolderAccessDecision,
  onFolderAccessCancel,
}: MessageListProps) {
  return (
    <div className="messages" ref={scrollRef} onScroll={onScroll}>
      {messages.length === 0 && folderAccessRequests.length === 0 && (
        <div className="message-notice" role="status">
          Configure a provider, pick a model, then send a message.
        </div>
      )}
      {messages.map((message) => (
        <MessageBubble
          key={message.id}
          message={message}
          busy={busy}
          onApproval={onApproval}
        />
      ))}
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
    </div>
  );
}

export function MessageBubble({
  message,
  busy,
  onApproval,
}: {
  message: ChatMessage;
  busy: boolean;
  onApproval: (callId: string, decision: "approve" | "reject") => void;
}) {
  if (message.role === "tool") {
    return <ToolCallCard name={message.name} status={message.status} />;
  }

  if (message.role === "approval") {
    return (
      <section className="message-approval" aria-label="Approval needed">
        <p>Approval needed: {message.summary}</p>
        {!message.resolved && (
          <div className="approval">
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => onApproval(message.callId, "approve")}
            >
              Approve
            </button>
            <button
              type="button"
              className="btn"
              onClick={() => onApproval(message.callId, "reject")}
            >
              Reject
            </button>
          </div>
        )}
      </section>
    );
  }

  if (message.role === "assistant") {
    return (
      <article className="message message-assistant" aria-label="Assistant">
        {message.text ? (
          <MessageMarkdown>{message.text}</MessageMarkdown>
        ) : busy ? (
          <span className="message-stream-placeholder" aria-label="Thinking">
            …
          </span>
        ) : null}
      </article>
    );
  }

  if (message.role === "user") {
    return (
      <article className="message message-user" aria-label="You">
        <MessageMarkdown>{message.text}</MessageMarkdown>
      </article>
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
