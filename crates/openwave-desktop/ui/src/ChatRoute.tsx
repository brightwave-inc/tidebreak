import { useEffect, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import type {
  CitationFormat,
  ModelInfo,
  ModelSelectionKey,
  PermissionMode,
  ReasoningEffort,
  SequencedEvent,
} from "./api";
import { useApp } from "./AppContext";
import { AssistantSourceMarkerStreamScrubber } from "./AssistantSourceMarkerStream";
import { ChatHeaderTitle } from "./ChatHeaderTitle";
import { reconcilePendingApprovalCards } from "./ApprovalHistory";
import { loadChatApprovalHydration } from "./ChatApprovalHydration";
import { useChatListStore } from "./ChatListStore";
import { ChatSessionController } from "./ChatSessionController";
import {
  applyTerminalHydration,
  type ChatSessionEffect,
  type ChatSessionState,
} from "./ChatSessionReducer";
import { useChatSessionStore } from "./ChatSessionStore";
import {
  loadCurrentTerminalTranscript,
  presentChatTranscript,
} from "./ChatTranscriptPresentation";
import { useFirstMessage } from "./FirstMessage";
import { ChatView } from "./ChatView";
import { OutputDetailRoot } from "./outputs/OutputDetailRoot";
import { OutputsView } from "./outputs/OutputsView";
import { DocumentDetailRoot } from "./document-detail/DocumentDetailRoot";
import { SourcesView } from "./sources/SourcesView";
import { FoldersView } from "./FoldersView";
import { hasNativeHost } from "./host";
import { attachChatFiles } from "./attachments";
import {
  type ImportedDocument,
  type LibraryImportSuccess,
} from "./documents";
import { DocumentDropTarget } from "./DocumentDropTarget";
import { ImportQueue } from "./ImportQueue";
import {
  readyImageAttachmentIds,
  readyTranscriptImageAttachments,
} from "./ImageAttachments";
import { useImageAttachments } from "./useImageAttachments";
import { modelForSelection } from "./ModelSelection";
import { ModelMenu, ReasoningEffortMenu } from "./ModelMenu";
import { PermissionModeMenu } from "./PermissionModeMenu";
import { ToolsMenu } from "./ToolsMenu";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "./NativePickerLatch";
import { PanelFrame } from "./panel/PanelFrame";
import { PanelLayout } from "./panel/PanelLayout";
import type { PanelContent } from "./panel/panelTypes";
import { usePanelNav } from "./panel/usePanelNav";
import { SourceNavProvider, useStableSourceNav } from "./panel/SourceNav";
import { RouteFrame } from "./RouteFrame";
import { ChatSidebar } from "./sidebar/ChatSidebar";
import { useRefreshSignals } from "./RefreshSignals";
import { TranscriptVisibilityProvider } from "./TranscriptVisibility";
import { useTurnLifecycle } from "./TurnLifecycleSignals";

let msgSeq = 0;

function nextId(): string {
  msgSeq += 1;
  return `m${msgSeq}`;
}

const sessionDeps = {
  nextId,
  now: () => new Date().toISOString(),
};

const chatListActions = useChatListStore.getState();
const firstMessageActions = useFirstMessage.getState();
const { signal: signalRefresh } = useRefreshSignals.getState();
const { signal: signalTurnLifecycle } = useTurnLifecycle.getState();

/**
 * One conversation and the panels arranged around it.
 *
 * The route is remounted per chat id, so nothing here survives a switch. That
 * is deliberate: everything scoped to a single conversation — its socket, its
 * transcript, its in-flight turn — is torn down by the unmount rather than
 * fenced off behind a generation counter.
 */
export function ChatRoute({ chatId }: { chatId: string }) {
  const navigate = useNavigate();
  const { client, models, defaultModelKey, defaultCitationFormat, setStatus } =
    useApp();
  const { layout, openPanel } = usePanelNav();
  const sourceNav = useStableSourceNav(openPanel);
  const chats = useChatListStore((state) => state.chats);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const busy = useChatSessionStore((session) => session.busy);
  const [hydrated, setHydrated] = useState(false);
  const [draft, setDraft] = useState("");
  const [attaching, setAttaching] = useState(false);
  const [recentSource, setRecentSource] = useState<ImportedDocument | null>(null);
  const [attachError, setAttachError] = useState<string | null>(null);
  const images = useImageAttachments(client, chatId);
  const handleEventRef = useRef<(event: SequencedEvent) => void>(() => {});
  const terminalHydrationGenerationRef = useRef(0);
  const draftRef = useRef("");

  const chat = chats.find((candidate) => candidate.id === chatId) ?? null;

  // A chat id that is not in the list — deleted in another window, or a stale
  // deep link — should land somewhere real rather than on an empty frame.
  useEffect(() => {
    if (chats.length > 0 && !chat) void navigate({ to: "/", replace: true });
  }, [chats.length, chat, navigate]);

  // A conversation opened from the home composer arrives with its first message
  // already written. `take` clears it, so a re-render cannot send it twice.
  useEffect(() => {
    if (!chat) return;
    const pending = firstMessageActions.take(chatId);
    if (pending) void sendMessage(pending);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chat, chatId]);

  useEffect(() => {
    let cancelled = false;
    setHydrated(false);
    useChatSessionStore.getState().reset();
    updateSession((session) => ({
      ...session,
      markerScrubber: new AssistantSourceMarkerStreamScrubber(),
    }));
    void (async () => {
      try {
        const hydration = await loadChatApprovalHydration(
          client,
          chatId,
          () => !cancelled,
        );
        if (!hydration) return;
        const { transcript, pendingApprovals } = hydration;
        const presented = presentChatTranscript(transcript);
        const pendingTurnId = pendingApprovals[0]?.turnId ?? null;
        updateSession((session) => ({
          ...session,
          lastSeq: transcript.last_event_seq,
          hydratedMessageIds: presented.messageIds,
          messages: reconcilePendingApprovalCards(presented.messages, pendingApprovals),
          activeTurnId: pendingTurnId,
          busy: pendingTurnId !== null,
        }));
        setHydrated(true);
      } catch (err) {
        if (cancelled) return;
        updateSession((session) => ({
          ...session,
          busy: true,
          messages: [
            {
              id: nextId(),
              role: "error",
              text: `Could not load this chat: ${String(err)}`,
            },
          ],
        }));
      }
    })();
    return () => {
      cancelled = true;
      terminalHydrationGenerationRef.current += 1;
    };
  }, [client, chatId]);

  useEffect(() => {
    if (!hydrated) return;
    const controller = new ChatSessionController({
      openSocket: (after, onFrame) => client.openEvents(chatId, after, onFrame),
      getAfter: () => useChatSessionStore.getState().lastSeq,
      onEvent: (event) => handleEventRef.current(event),
      onMetadata: (metadata) =>
        chatListActions.applyDerivedTitle(chatId, metadata.title),
      onConnectionState: (connectionState) =>
        setStatus((current) => `${withoutConnectionState(current)} · ${connectionState}`),
    });
    controller.start();
    return () => {
      controller.dispose();
      // Leaving the conversation settles its name: coming back to it should show
      // the title, not type it out a second time.
      chatListActions.clearDerivedTitle();
    };
  }, [client, chatId, hydrated, setStatus]);

  function updateSession(update: (state: ChatSessionState) => ChatSessionState) {
    useChatSessionStore.getState().update(update);
  }

  function handleEvent(framed: SequencedEvent) {
    const effects = useChatSessionStore.getState().applyEvent(framed, sessionDeps);
    for (const effect of effects) applySessionEffect(effect);
  }
  handleEventRef.current = handleEvent;

  function applySessionEffect(effect: ChatSessionEffect) {
    switch (effect.type) {
      case "refresh_folder_access":
        signalRefresh("folderAccess");
        return;
      case "refresh_output_writebacks":
        signalRefresh("outputWritebacks");
        return;
      case "refresh_user_questions":
        signalRefresh("userQuestions");
        return;
      case "turn_began":
        signalTurnLifecycle(effect.startsDifferentTurn ? "began" : "began_same_turn");
        return;
      case "turn_resolved":
        signalTurnLifecycle("resolved");
        return;
      case "invalidate_terminal_hydration":
        terminalHydrationGenerationRef.current += 1;
        return;
      case "hydrate_terminal_transcript": {
        const generation = ++terminalHydrationGenerationRef.current;
        void refreshTerminalTranscript(generation);
        return;
      }
    }
  }

  async function refreshTerminalTranscript(generation: number) {
    try {
      const presented = await loadCurrentTerminalTranscript(
        client,
        chatId,
        () => terminalHydrationGenerationRef.current === generation,
      );
      if (!presented) return;
      updateSession((session) => applyTerminalHydration(session, presented));
    } catch {
      // The scrubbed optimistic response remains safe and visible. Reopening
      // the conversation will load a fresh authoritative snapshot.
    }
  }

  function setComposerDraft(next: string) {
    draftRef.current = next;
    setDraft(next);
  }

  async function onSend() {
    await sendMessage(draft.trim());
  }

  /**
   * The one path a message takes. Home writes the first message of a new chat
   * but does not post it, so this has to be reachable with text that was never
   * in this route's draft.
   */
  async function sendMessage(content: string) {
    if (!chat || !content || busy || deletingChatId !== null) return;
    const attachments = readyImageAttachmentIds(images.attachments);
    const transcriptImages = readyTranscriptImageAttachments(images.attachments);
    const turnId = crypto.randomUUID();
    terminalHydrationGenerationRef.current += 1;
    setComposerDraft("");
    updateSession((session) => ({
      ...session,
      busy: true,
      activeTurnId: turnId,
      messages: [
        ...session.messages,
        {
          id: nextId(),
          role: "user",
          text: content,
          images: transcriptImages,
          createdAt: new Date().toISOString(),
        },
      ],
    }));
    signalTurnLifecycle("submitted");
    try {
      await client.postMessage(chatId, turnId, content, attachments);
      setRecentSource(null);
      // Only once the turn is durably accepted. A refused send — an image the
      // selected model cannot read, say — must leave the attachments where the
      // reader can fix the problem and try again.
      images.clear();
    } catch (err) {
      updateSession((session) => ({
        ...session,
        busy: false,
        activeTurnId: null,
        messages: [...session.messages, { id: nextId(), role: "error", text: String(err) }],
      }));
      signalTurnLifecycle("resolved");
    }
  }

  /**
   * One picker for anything the reader wants to attach.
   *
   * Which of the two things each file becomes — pixels for the model, or a
   * parsed and searchable source — is decided by the host from the bytes, so
   * nothing here has to guess from a name or ask the reader to know first.
   */
  async function onAttach() {
    if (attaching || deletingChatId !== null) return;
    if (!useNativePickerLatch.getState().claim(PICKER_HOLDERS.importSource)) {
      setAttachError(PICKER_BUSY_MESSAGE);
      return;
    }
    setAttaching(true);
    setAttachError(null);
    try {
      const attached = await attachChatFiles(chatId);
      if (!attached) return;
      images.adopt(attached.images);
      const source = attached.documents?.results.find(isImportedDocument);
      if (source) setRecentSource(source.document);
      // A file that could not be attached as an image is the reader's to fix,
      // and saying nothing would read as a silent drop from the selection.
      const [firstFailure] = attached.failedImages;
      if (firstFailure) {
        setAttachError(`${firstFailure.fileName}: ${firstFailure.message}`);
      }
    } catch (err) {
      setAttachError(friendlyAttachError(err));
    } finally {
      useNativePickerLatch.getState().release(PICKER_HOLDERS.importSource);
      setAttaching(false);
    }
  }

  async function onModelChange(modelId: ModelSelectionKey | null) {
    if (deletingChatId !== null) return;
    chatListActions.replaceChat(await client.patchChatModel(chatId, modelId || null));
  }

  async function onReasoningEffortChange(effort: ReasoningEffort | null) {
    if (deletingChatId !== null) return;
    chatListActions.replaceChat(await client.patchChatReasoningEffort(chatId, effort));
  }

  async function onPermissionModeChange(mode: PermissionMode) {
    if (deletingChatId !== null) return;
    chatListActions.replaceChat(await client.patchChatPermissionMode(chatId, mode));
  }

  async function onCitationFormatChange(format: CitationFormat | null) {
    if (deletingChatId !== null) return;
    chatListActions.replaceChat(await client.patchChatCitationFormat(chatId, format));
  }

  if (!chat) return <div className="routed-surface-loading" />;

  function renderPanel(panel: PanelContent, position: "left" | "right" | "chat", visible: boolean) {
    if (panel.type === "chat") {
      // Only the levels the selected model accepts are offerable, and a model
      // that accepts none gets no selector at all.
      const efforts =
        modelForSelection(models, chat!.model)?.reasoning_efforts ?? [];
      return (
        <TranscriptVisibilityProvider value={visible}>
          <ChatView
            client={client}
            chat={chat!}
            hydrated={hydrated}
            nativeHost={hasNativeHost()}
            deletingChat={deletingChatId !== null}
            draft={draft}
            draftRef={draftRef}
            attachedSourceName={recentSource?.displayName ?? null}
            attachError={attachError}
            composerImages={{
              items: images.attachments,
              error: images.error,
              unsupportedModel: textOnlyModelLabel(models, chat!.model),
              onAttachFiles: images.attachFiles,
              onRemove: images.remove,
              onRetry: images.retry,
            }}
            composerModelMenu={
              <>
                <ToolsMenu
                  disabled={deletingChatId !== null}
                  onAttach={hasNativeHost() ? onAttach : undefined}
                  attaching={attaching}
                  citationFormat={chat!.citation_format}
                  defaultCitationFormat={defaultCitationFormat}
                  onCitationFormatChange={onCitationFormatChange}
                />
                <ModelMenu
                  models={models}
                  value={chat!.model}
                  defaultKey={defaultModelKey}
                  disabled={deletingChatId !== null}
                  onChange={onModelChange}
                />
                {efforts.length > 0 && (
                  <ReasoningEffortMenu
                    levels={efforts}
                    value={chat!.reasoning_effort}
                    disabled={deletingChatId !== null}
                    onChange={onReasoningEffortChange}
                  />
                )}
                <PermissionModeMenu
                  value={chat!.permission_mode}
                  disabled={deletingChatId !== null}
                  onChange={onPermissionModeChange}
                />
              </>
            }
            onDraftChange={setComposerDraft}
            onDismissAttachedSource={() => setRecentSource(null)}
            onSelectPrompt={setComposerDraft}
            onSend={onSend}
            onViewOutput={() => openPanel({ type: "outputs" })}
          />
        </TranscriptVisibilityProvider>
      );
    }

    const side = position === "right" ? "right" : "left";
    switch (panel.type) {
      case "sources":
        // A document id turns the list into the reader for that one source; the
        // breadcrumb's way back clears the id and lands on the catalog again.
        return panel.documentId ? (
          <DocumentDetailRoot
            chatId={chatId}
            documentID={panel.documentId}
            citationId={panel.citationId}
            position={side}
          />
        ) : (
          <PanelFrame position={side} spaceBetween>
            <SourcesView
              chatId={chatId}
              onOpen={(documentId) => openPanel({ type: "sources", documentId })}
            />
          </PanelFrame>
        );
      case "outputs":
        // An output id turns the list into the reader for that one output, the
        // same way a document id does for sources.
        return panel.outputId ? (
          <OutputDetailRoot
            chatId={chatId}
            outputId={panel.outputId}
            position={side}
          />
        ) : (
          <PanelFrame position={side} spaceBetween>
            <OutputsView
              chatId={chatId}
              onOpen={(outputId) => openPanel({ type: "outputs", outputId })}
            />
          </PanelFrame>
        );
      case "folders":
        return (
          <PanelFrame position={side} spaceBetween>
            <FoldersView chat={chat!} />
          </PanelFrame>
        );
    }
  }

  return (
    <RouteFrame sidebar={<ChatSidebar chat={chat} />}>
    <div className="mr-2 flex min-h-0 min-w-0 flex-1 flex-col">
      <header className="mt-2 flex h-9 w-full shrink-0 items-center justify-between gap-2 pl-4 pr-1">
        <ChatHeaderTitle chat={chat} />
      </header>
      {/* Citations live in the transcript but open into the panel beside it,
          so the way there is provided above both slots. */}
      <SourceNavProvider value={sourceNav}>
        <PanelLayout layout={layout} renderPanel={renderPanel} />
      </SourceNavProvider>
      <ImportQueue />
      <DocumentDropTarget chatId={chatId} />
    </div>
    </RouteFrame>
  );
}

function withoutConnectionState(status: string): string {
  return status.replace(/ · (?:live|reconnecting)$/, "");
}

/**
 * The label of the chat's model when it cannot read images, or `null`.
 *
 * A chat with no model of its own follows the global default, which the
 * renderer does not resolve; the server still refuses such a turn, so the
 * composer stays quiet rather than guessing at a name it would have to print.
 */
function textOnlyModelLabel(
  models: ModelInfo[],
  selection: string | null,
): string | null {
  const model = modelForSelection(models, selection);
  return model && !model.multimodal ? model.display_name : null;
}

function friendlyAttachError(error: unknown): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : "Could not attach that file.";
}

function isImportedDocument(
  result: { status: string },
): result is LibraryImportSuccess {
  return result.status === "imported" || result.status === "already_present";
}
