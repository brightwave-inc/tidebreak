import { useEffect, useRef, useState, type ReactNode } from "react";
import type {
  AgentRun,
  Chat,
  PendingFolderAccessRequest,
} from "./api";
import { AgentActivityPanel } from "./AgentActivityPanel";
import { ChatTabs } from "./ChatTabs";
import { isNearBottom, scrollToLatest } from "./ChatScroll";
import { useChatSessionStore } from "./ChatSessionStore";
import { useUiStore } from "./UiStore";
import { Composer } from "./Composer";
import type { FolderAccessDecision } from "./host";
import { MessageList } from "./MessageList";
import { ArrowDown, Settings } from "lucide-react";

export type ChatViewProps = {
  chat: Chat;
  status: string;
  hydrated: boolean;
  nativeHost: boolean;
  deletingChat: boolean;
  agentRuns: AgentRun[];
  agentRunsLoading: boolean;
  agentRunsError: string | null;
  stoppingRunIds: Set<string>;
  stopErrorRunIds: Set<string>;
  onRetryAgentRuns: () => void;
  onStopSandboxRun: (runId: string) => void;
  folderAccessRequests: PendingFolderAccessRequest[];
  resolvingFolderCalls: Set<string>;
  folderAccessErrors: Record<string, string>;
  decidingApprovalCalls: Set<string>;
  approvalErrors: Record<string, string>;
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
  draft: string;
  composerModelMenu: ReactNode;
  attachingSource: boolean;
  attachedSourceName: string | null;
  sourceAttachmentError: string | null;
  cancelError: string | null;
  cancelPendingTurnId: string | null;
  steerError: string | null;
  steerStatus: string | null;
  steerPendingTurnId: string | null;
  onDraftChange: (value: string) => void;
  onAddSource: () => Promise<void>;
  onDismissAttachedSource: () => void;
  onSelectPrompt: (prompt: string) => void;
  onSend: () => Promise<void>;
  onSteer: () => Promise<void>;
  onStop: () => Promise<void>;
};

/**
 * The chat pane: header, agent activity, transcript, and composer. Reads the
 * live session (messages, busy, active turn) straight from the session store.
 * Mount with `key={chat.id}` so scroll-follow state resets per conversation.
 */
export function ChatView({
  chat,
  status,
  hydrated,
  nativeHost,
  deletingChat,
  agentRuns,
  agentRunsLoading,
  agentRunsError,
  stoppingRunIds,
  stopErrorRunIds,
  onRetryAgentRuns,
  onStopSandboxRun,
  folderAccessRequests,
  resolvingFolderCalls,
  folderAccessErrors,
  decidingApprovalCalls,
  approvalErrors,
  onApproval,
  onFolderAccessDecision,
  onFolderAccessCancel,
  draft,
  composerModelMenu,
  attachingSource,
  attachedSourceName,
  sourceAttachmentError,
  cancelError,
  cancelPendingTurnId,
  steerError,
  steerStatus,
  steerPendingTurnId,
  onDraftChange,
  onAddSource,
  onDismissAttachedSource,
  onSelectPrompt,
  onSend,
  onSteer,
  onStop,
}: ChatViewProps) {
  const showSettings = useUiStore((state) => state.showSettings);
  const messages = useChatSessionStore((session) => session.messages);
  const busy = useChatSessionStore((session) => session.busy);
  const activeTurnId = useChatSessionStore((session) => session.activeTurnId);
  const reasoningActive = useChatSessionStore(
    (session) => session.reasoningActive,
  );

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const followsLatestRef = useRef(true);
  const visibleFolderCallIdsRef = useRef<Set<string>>(new Set());
  const [hasUnreadActivity, setHasUnreadActivity] = useState(false);

  useEffect(() => {
    const scroll = scrollRef.current;
    if (!scroll) return;
    if (followsLatestRef.current) {
      scrollToLatest(scroll);
    } else {
      setHasUnreadActivity(true);
    }
  }, [messages]);

  useEffect(() => {
    const next = new Set(folderAccessRequests.map((request) => request.callId));
    const gainedRequest = [...next].some(
      (callId) => !visibleFolderCallIdsRef.current.has(callId),
    );
    visibleFolderCallIdsRef.current = next;
    if (!gainedRequest) return;
    const scroll = scrollRef.current;
    if (!scroll) return;
    if (followsLatestRef.current) {
      scrollToLatest(scroll);
    } else {
      setHasUnreadActivity(true);
    }
  }, [folderAccessRequests]);

  return (
    <section className="chat-pane">
      <header className="conversation-header">
        <div className="conversation-title-row">
          <h1>{chat.title?.trim() || "New chat"}</h1>
        </div>
        {nativeHost && <ChatTabs />}
        <div className="conversation-header-actions">
          <div className="mobile-settings-actions">
            <button
              type="button"
              className="btn"
              aria-label="Settings"
              onClick={showSettings}
            >
              <Settings size={14} />
            </button>
          </div>
          <span className="status" title={status}>
            {status}
          </span>
        </div>
      </header>

      <AgentActivityPanel
        runs={agentRuns}
        loading={agentRunsLoading}
        error={agentRunsError}
        onRetry={onRetryAgentRuns}
        stoppingRunIds={stoppingRunIds}
        stopErrorRunIds={stopErrorRunIds}
        onStop={onStopSandboxRun}
      />

      <div className="message-view">
        <MessageList
          messages={messages}
          folderAccessRequests={folderAccessRequests}
          nativeHost={nativeHost}
          nativeBusy={resolvingFolderCalls.size > 0}
          resolvingFolderCalls={resolvingFolderCalls}
          folderAccessErrors={folderAccessErrors}
          decidingApprovalCalls={decidingApprovalCalls}
          approvalErrors={approvalErrors}
          busy={busy}
          reasoningActive={reasoningActive}
          scrollRef={scrollRef}
          onScroll={(event) => {
            const followsLatest = isNearBottom(event.currentTarget);
            followsLatestRef.current = followsLatest;
            if (followsLatest) setHasUnreadActivity(false);
          }}
          onApproval={onApproval}
          onFolderAccessDecision={onFolderAccessDecision}
          onFolderAccessCancel={onFolderAccessCancel}
          onSelectPrompt={onSelectPrompt}
          hydrated={hydrated}
        />
        {hasUnreadActivity && (
          <button
            type="button"
            className="new-activity"
            onClick={() => {
              followsLatestRef.current = true;
              setHasUnreadActivity(false);
              if (scrollRef.current) scrollToLatest(scrollRef.current);
            }}
          >
            New activity
            <ArrowDown size={13} />
          </button>
        )}
      </div>

      <Composer
        activeTurnId={activeTurnId}
        busy={busy}
        cancelError={cancelError}
        cancelPending={
          activeTurnId !== null && cancelPendingTurnId === activeTurnId
        }
        disabled={deletingChat}
        draft={draft}
        modelMenu={composerModelMenu}
        canAttachSource={nativeHost}
        attachingSource={attachingSource}
        attachedSourceName={attachedSourceName}
        sourceAttachmentError={sourceAttachmentError}
        onAddSource={onAddSource}
        onDismissAttachedSource={onDismissAttachedSource}
        onDraftChange={onDraftChange}
        onSend={onSend}
        onSteer={onSteer}
        onStop={onStop}
        resetKey={chat.id}
        steerError={steerError}
        steerPending={
          activeTurnId !== null && steerPendingTurnId === activeTurnId
        }
        steerStatus={steerStatus}
      />
    </section>
  );
}
