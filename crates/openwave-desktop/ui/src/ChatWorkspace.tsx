import { useSyncExternalStore, type ReactNode } from "react";
import { Maximize2, Minimize2, Settings, X } from "lucide-react";
import type { Chat } from "./api";
import { ChatTabs, requiresNativeHost } from "./ChatTabs";
import { CHAT_SURFACE, type Surface } from "./Surface";
import { DeliverablesView } from "./DeliverablesView";
import { DocumentsView } from "./DocumentsView";
import { FoldersView } from "./FoldersView";
import { PanelResizer } from "./PanelResizer";
import { TranscriptVisibilityProvider } from "./TranscriptVisibility";
import { opensBesideTranscript, resolveSlots } from "./WorkspaceLayout";
import { useUiStore } from "./UiStore";

/** Below this the workspace shows one surface at a time. */
const NARROW_QUERY = "(max-width: 720px)";

export type ChatWorkspaceProps = {
  chat: Chat;
  status: string;
  nativeHost: boolean;
  /** The transcript surface, supplied by the owner that holds session state. */
  transcript: ReactNode;
};

/**
 * The workspace for the selected chat: one header carrying the conversation
 * title, the surface switcher, and status, over a body that holds the
 * transcript and, beside it, whichever scoped surface is open.
 *
 * The transcript stays mounted while a surface is open. That is the point of
 * the arrangement — a conversation's own sources should be readable without
 * leaving the conversation, and detail can only move out of the transcript if
 * there is somewhere beside it to put it.
 */
export function ChatWorkspace({
  chat,
  status,
  nativeHost,
  transcript,
}: ChatWorkspaceProps) {
  const selected = useUiStore((state) => state.surface);
  const expanded = useUiStore((state) => state.expanded);
  const fraction = useUiStore((state) => state.fraction);
  const showChat = useUiStore((state) => state.showChat);
  const showSettings = useUiStore((state) => state.showSettings);
  const toggleExpanded = useUiStore((state) => state.toggleExpanded);
  const setFraction = useUiStore((state) => state.setFraction);
  const narrow = useNarrowWorkspace();

  // A surface the host cannot serve is offered as disabled in the switcher, so
  // reaching one here means the host went away underneath the selection.
  const surface =
    !nativeHost && requiresNativeHost(selected.kind) ? CHAT_SURFACE : selected;
  const slots = resolveSlots({ surface, expanded, fraction, narrow });
  const panelOpen = opensBesideTranscript(surface.kind);

  return (
    <div className="chat-workspace">
      <header className="conversation-header">
        <div className="conversation-title-row">
          <h1>{chat.title?.trim() || "New chat"}</h1>
        </div>
        <div className="conversation-switcher">
          <ChatTabs nativeHost={nativeHost} />
          {panelOpen && !narrow && (
            <div className="panel-controls">
              <button
                type="button"
                className="panel-control"
                aria-label={expanded ? "Show the transcript" : "Expand panel"}
                aria-pressed={expanded}
                title={expanded ? "Show the transcript" : "Expand panel"}
                onClick={toggleExpanded}
              >
                {expanded ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
              </button>
              <button
                type="button"
                className="panel-control"
                aria-label="Close panel"
                title="Close panel"
                onClick={showChat}
              >
                <X size={14} />
              </button>
            </div>
          )}
        </div>
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

      <div className="workspace-body">
        {/*
          Hidden rather than unmounted: an open panel must not cost the reader
          their scroll position, and the transcript keeps streaming behind it.
        */}
        <div
          className="workspace-slot workspace-transcript"
          hidden={!slots.showTranscript}
          style={
            slots.showPanel
              ? { flex: `1 1 ${(1 - slots.fraction) * 100}%` }
              : undefined
          }
        >
          <TranscriptVisibilityProvider value={slots.showTranscript}>
            {transcript}
          </TranscriptVisibilityProvider>
        </div>

        {slots.showTranscript && slots.showPanel && (
          <PanelResizer fraction={slots.fraction} onFraction={setFraction} />
        )}

        {slots.showPanel && (
          <div
            className={`workspace-slot workspace-panel${
              slots.showTranscript ? " is-sharing" : ""
            }`}
            style={
              slots.showTranscript
                ? { flex: `0 0 ${slots.fraction * 100}%` }
                : undefined
            }
          >
            <ScopedSurface chat={chat} surface={surface} />
          </div>
        )}
      </div>
    </div>
  );
}

function ScopedSurface({ chat, surface }: { chat: Chat; surface: Surface }) {
  switch (surface.kind) {
    case "documents":
      return <DocumentsView chatId={chat.id} />;
    case "deliverables":
      return (
        <DeliverablesView chatId={chat.id} initialFilename={surface.itemId} />
      );
    case "folders":
      return <FoldersView chat={chat} />;
    default:
      return null;
  }
}

function useNarrowWorkspace(): boolean {
  return useSyncExternalStore(subscribeToNarrow, isNarrow, () => false);
}

function subscribeToNarrow(onChange: () => void): () => void {
  if (typeof window === "undefined" || !window.matchMedia) return () => {};
  const query = window.matchMedia(NARROW_QUERY);
  query.addEventListener("change", onChange);
  return () => query.removeEventListener("change", onChange);
}

function isNarrow(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return false;
  return window.matchMedia(NARROW_QUERY).matches;
}
