import { useEffect, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import type {
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
import {
  useComposerAttachments,
  useComposerDraft,
  useComposerDrafts,
} from "./ComposerDrafts";
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
import type { RetryableTurn } from "./MessageList";
import type { TranscriptFileAttachment } from "./TranscriptFileAttachments";
import type { TranscriptImageAttachment } from "./ImageAttachments";
import { OutputDetailRoot } from "./outputs/OutputDetailRoot";
import { OutputsView } from "./outputs/OutputsView";
import { DocumentDetailRoot } from "./document-detail/DocumentDetailRoot";
import { FoldersView } from "./FoldersView";
import { hasNativeHost } from "./host";
import { attachChatFiles, type AttachedFiles } from "./attachments";
import { type ImportedDocument, type LibraryImportSuccess } from "./documents";
import { DocumentDropTarget } from "./DocumentDropTarget";
import {
  MAX_IMAGE_ATTACHMENTS,
  readyImageAttachmentIds,
  readyTranscriptImageAttachments,
  type ImageAttachment,
} from "./ImageAttachments";
import { useImageAttachments } from "./useImageAttachments";
import { modelForSelection } from "./ModelSelection";
import { ModelMenu, ReasoningEffortMenu } from "./ModelMenu";
import { PermissionModeMenu } from "./PermissionModeMenu";
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
import { useChatFolderAttachments } from "./useChatFolderAttachments";

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
const composerDraftActions = useComposerDrafts.getState();
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
  const { client, models, defaultModelKey, setStatus } = useApp();
  const { layout, openPanel } = usePanelNav();
  const sourceNav = useStableSourceNav(openPanel);
  const chats = useChatListStore((state) => state.chats);
  const chatsLoaded = useChatListStore((state) => state.chatsLoaded);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const busy = useChatSessionStore((session) => session.busy);
  const [hydrated, setHydrated] = useState(false);
  const draft = useComposerDraft(chatId);
  const files = useComposerAttachments(chatId).files;
  const [attaching, setAttaching] = useState(false);
  const [attachError, setAttachError] = useState<string | null>(null);
  const images = useImageAttachments(client, chatId);
  const handleEventRef = useRef<(event: SequencedEvent) => void>(() => {});
  const terminalHydrationGenerationRef = useRef(0);
  // Steering reads the draft synchronously, from outside a render.
  const draftRef = useRef(draft);
  draftRef.current = draft;

  const chat = chats.find((candidate) => candidate.id === chatId) ?? null;
  const nativeHost = hasNativeHost();
  const folders = useChatFolderAttachments(chat, nativeHost);

  // A chat id that is not in the list — deleted in another window, or a stale
  // deep link — should land somewhere real rather than on an empty frame. The
  // gate is whether the list has been fetched, not whether it has rows: an
  // account with no chats left is exactly the case that would otherwise sit on
  // the loading frame forever.
  useEffect(() => {
    if (chatsLoaded && !chat) void navigate({ to: "/", replace: true });
  }, [chatsLoaded, chat, navigate]);

  // A conversation opened from the home composer arrives with its first message
  // already written. Wait for the empty chat's authoritative snapshot before
  // appending it: hydration replaces the session transcript, so sending during
  // the first mount pass would let the reset below erase the optimistic bubble.
  // `take` clears it, so a re-render cannot send it twice.
  useEffect(() => {
    if (!chat || !hydrated) return;
    const pending = firstMessageActions.take(chatId);
    if (pending) void sendMessage(pending.text, pending.images, pending.files);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chat, chatId, hydrated]);

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
      case "refresh_plan_approvals":
        signalRefresh("planApprovals");
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
    composerDraftActions.setDraft(chatId, next);
  }

  function setComposerFiles(
    update: (current: readonly ImportedDocument[]) => ImportedDocument[],
  ) {
    const current =
      useComposerDrafts.getState().attachments[chatId]?.files ?? [];
    composerDraftActions.setFiles(chatId, update(current));
  }

  async function onSend() {
    await sendMessage(draft.trim());
  }

  /**
   * The one path a message takes. Home writes the first message of a new chat
   * but does not post it, so this has to be reachable with text that was never
   * in this route's draft.
   */
  async function sendMessage(
    content: string,
    imageItems: readonly ImageAttachment[] = images.attachments,
    fileItems: readonly ImportedDocument[] = files,
  ) {
    await postTurn({
      content,
      attachments: readyImageAttachmentIds(imageItems),
      transcriptImages: readyTranscriptImageAttachments(imageItems),
      documentIds: fileItems.map((file) => file.documentId),
      transcriptFiles: fileItems.map((file) => ({
        documentId: file.documentId,
        name: file.displayName,
        mediaType: file.mediaType,
      })),
      fromComposer: true,
    });
  }

  /**
   * Retry sends the failed turn again — same prompt, same attachments, a new
   * turn id.
   *
   * There is no server-side resume: a failed turn is terminal in the journal
   * and nothing re-runs one in place. A fresh turn is exactly what the reader
   * would get by retyping the prompt, without the retyping, and it reuses the
   * attachment and document ids the first attempt published, so the model sees
   * the same message rather than a text-only shadow of it.
   */
  function retryTurn(turn: RetryableTurn) {
    void postTurn({
      content: turn.text,
      attachments: turn.images.map((image) => image.attachmentId),
      transcriptImages: [...turn.images],
      documentIds: turn.files.map((file) => file.documentId),
      transcriptFiles: [...turn.files],
      fromComposer: false,
    });
  }

  /**
   * The one path a turn takes to the server, whether the reader typed it or the
   * retry button resent it. `fromComposer` is what separates the two: only a
   * send that drew on the composer may empty it.
   */
  async function postTurn({
    content,
    attachments,
    transcriptImages,
    documentIds,
    transcriptFiles,
    fromComposer,
  }: {
    content: string;
    attachments: readonly string[];
    transcriptImages: readonly TranscriptImageAttachment[];
    documentIds: readonly string[];
    transcriptFiles: readonly TranscriptFileAttachment[];
    fromComposer: boolean;
  }) {
    if (!chat || !content || busy || deletingChatId !== null) return;
    const turnId = crypto.randomUUID();
    terminalHydrationGenerationRef.current += 1;
    const optimisticId = nextId();
    if (fromComposer) setComposerDraft("");
    updateSession((session) => ({
      ...session,
      busy: true,
      activeTurnId: turnId,
      messages: [
        ...session.messages,
        {
          id: optimisticId,
          role: "user",
          text: content,
          images: [...transcriptImages],
          files: [...transcriptFiles],
          createdAt: new Date().toISOString(),
        },
      ],
    }));
    signalTurnLifecycle("submitted");
    try {
      await client.postMessage(
        chatId,
        turnId,
        content,
        attachments,
        documentIds,
      );
      // Only once the turn is durably accepted, and only for what the composer
      // actually contributed. A refused send — an image the selected model
      // cannot read, say — must leave the attachments where the reader can fix
      // the problem and try again; a retry must not throw away attachments the
      // reader has queued for their *next* message.
      if (fromComposer) {
        images.clear();
        setComposerFiles(() => []);
      }
    } catch (err) {
      // Nothing was accepted, so the message has to go back to where it can be
      // fixed and sent again: the text returns to the composer and the
      // optimistic bubble — which no turn will ever answer — comes out of the
      // transcript. Attachments are already left in place for the same reason.
      updateSession((session) => ({
        ...session,
        busy: false,
        activeTurnId: null,
        messages: [
          ...session.messages.filter((message) => message.id !== optimisticId),
          { id: nextId(), role: "error", text: String(err) },
        ],
      }));
      if (!draftRef.current) setComposerDraft(content);
      signalTurnLifecycle("resolved");
    }
  }

  /**
   * One picker for anything the reader wants to attach.
   *
   * Which of the two things each file becomes — pixels for the model, or a
   * parsed and readable source — is decided by the host from the bytes, so
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
      adoptAttached(attached);
    } catch (err) {
      setAttachError(friendlyAttachError(err));
    } finally {
      useNativePickerLatch.getState().release(PICKER_HOLDERS.importSource);
      setAttaching(false);
    }
  }

  function adoptAttached(attached: AttachedFiles) {
    const seenDocumentIds = new Set(files.map((file) => file.documentId));
    const imported =
      attached.documents?.results
        .filter(isImportedDocument)
        .map((result) => result.document)
        .filter((document) => {
          if (seenDocumentIds.has(document.documentId)) return false;
          seenDocumentIds.add(document.documentId);
          return true;
        }) ?? [];
    const remaining =
      MAX_IMAGE_ATTACHMENTS - images.attachments.length - files.length;
    const imagesToAdopt = attached.images.slice(0, Math.max(0, remaining));
    const filesToAdopt = imported.slice(
      0,
      Math.max(0, remaining - imagesToAdopt.length),
    );
    images.adopt(imagesToAdopt);
    if (filesToAdopt.length > 0) {
      setComposerFiles((current) => [...current, ...filesToAdopt]);
    }
    if (
      imagesToAdopt.length + filesToAdopt.length <
      attached.images.length + imported.length
    ) {
      setAttachError(
        `A message can carry at most ${MAX_IMAGE_ATTACHMENTS} attachments.`,
      );
    }
    const failedDocument = attached.documents?.results.find(
      (result) => result.status === "failed",
    );
    if (failedDocument?.status === "failed") {
      setAttachError(`${failedDocument.displayName}: ${failedDocument.message}`);
    }
    const [failedImage] = attached.failedImages;
    if (failedImage) {
      setAttachError(`${failedImage.fileName}: ${failedImage.message}`);
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
            nativeHost={nativeHost}
            deletingChat={deletingChatId !== null}
            draft={draft}
            draftRef={draftRef}
            attachError={attachError}
            files={{
              items: files,
              attaching,
              onAttach: hasNativeHost() ? onAttach : undefined,
              onRemove: (documentId) =>
                setComposerFiles((current) =>
                  current.filter((file) => file.documentId !== documentId),
                ),
            }}
            folders={{
              items: folders.items,
              working: folders.working,
              error: folders.error,
              onAttach: nativeHost ? folders.attach : undefined,
              onRemove: folders.remove,
            }}
            nativeDropTarget={
              <DocumentDropTarget
                chatId={chatId}
                onAttached={adoptAttached}
                onError={(error) => setAttachError(friendlyAttachError(error))}
              />
            }
            composerImages={{
              items: images.attachments,
              error: images.error,
              unsupportedModel: textOnlyModelLabel(models, chat!.model),
              onAttachFiles: (selected) => {
                if (
                  images.attachments.length + files.length + selected.length >
                  MAX_IMAGE_ATTACHMENTS
                ) {
                  setAttachError(
                    `A message can carry at most ${MAX_IMAGE_ATTACHMENTS} attachments.`,
                  );
                  return;
                }
                images.attachFiles(selected);
              },
              onRemove: images.remove,
              onRetry: images.retry,
            }}
            composerModelMenu={
              <>
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
            onSelectPrompt={setComposerDraft}
            onSend={onSend}
            onRetryTurn={retryTurn}
            onViewOutput={() => openPanel({ type: "outputs" })}
          />
        </TranscriptVisibilityProvider>
      );
    }

    const side = position === "right" ? "right" : "left";
    switch (panel.type) {
      case "document":
        return (
          <DocumentDetailRoot
            chatId={chatId}
            documentID={panel.documentId}
            citationId={panel.citationId}
            position={side}
          />
        );
      case "outputs":
        // An output id turns the list into the reader for that one output.
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
