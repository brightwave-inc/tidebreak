import { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";

import type {
  ModelSelectionKey,
  NetworkPolicy,
  PermissionMode,
  ReasoningEffort,
  SequencedEvent,
} from "./api";
import { useApp } from "./AppContext";
import { AssistantSourceMarkerStreamScrubber } from "./AssistantSourceMarkerStream";
import { ChatHeaderTitle } from "./ChatHeaderTitle";
import { summedTurnTokens } from "./ContextUsage";
import { ChatStatusChip } from "./ChatStatusChip";
import {
  loadChatApprovalHydration,
  sessionFromOpenedChat,
} from "./ChatApprovalHydration";
import { useChatListStore } from "./ChatListStore";
import { useComposerAttachments, useComposerDrafts } from "./ComposerDrafts";
import { ChatSessionController } from "./ChatSessionController";
import {
  applyTerminalHydration,
  type ChatSessionEffect,
  type ChatSessionState,
} from "./ChatSessionReducer";
import { useChatSessionStore } from "./ChatSessionStore";
import { loadCurrentTerminalTranscript } from "./ChatTranscriptPresentation";
import { useFirstMessage } from "./FirstMessage";
import { ChatView } from "./ChatView";
import type { RetryableTurn } from "./MessageList";
import type { TranscriptFileAttachment } from "./TranscriptFileAttachments";
import type { TranscriptImageAttachment } from "./ImageAttachments";
import { AgentsPanel } from "./AgentsPanel";
import { PermissionsPanel } from "./settings/PermissionsPanel";
import { BackgroundAgentPanel } from "./BackgroundAgentPanel";
import { OutputDetailRoot } from "./outputs/OutputDetailRoot";
import { OutputsView } from "./outputs/OutputsView";
import { DocumentDetailRoot } from "./document-detail/DocumentDetailRoot";
import { warmPresentationConverter } from "./document/officePdf";
import { FoldersView } from "./FoldersView";
import { hasLocalHostAuthority, hasNativeHost } from "./host";
import { Skeleton } from "@/components/ui/skeleton";
import { usePortalOverlayOpen } from "@/lib/usePortalOverlayOpen";
import { foregroundBrowserScope } from "./code/browser/foregroundBrowserScope";
import { seedBrowserSession } from "./code/browser/browserPersistence";

import { friendlyErrorMessage } from "./lib/utils";
import {
  attachChatFiles,
  attachHeldChatFiles,
  pickHeldFiles,
  type AttachedFiles,
} from "./attachments";
import { type ImportedDocument, type LibraryImportSuccess } from "./documents";
import { DocumentDropTarget } from "./DocumentDropTarget";
import {
  MAX_IMAGE_ATTACHMENTS,
  readyImageAttachmentIds,
  readyTranscriptImageAttachments,
  type ImageAttachment,
} from "./ImageAttachments";
import { useImageAttachments } from "./useImageAttachments";
import { modelForChat, textOnlyModelLabel } from "./ModelSelection";
import { ModelMenu, useModelSettingsNav } from "./ModelMenu";
import { PermissionModeMenu } from "./PermissionModeMenu";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "./NativePickerLatch";
import { PinnedOutputsStrip } from "./PinnedOutputsStrip";
import { PanelFrame } from "./panel/PanelFrame";
import { PanelLayout } from "./panel/PanelLayout";
import type { PanelContent } from "./panel/panelTypes";
import { usePanelNav } from "./panel/usePanelNav";
import { SourceNavProvider, useStableSourceNav } from "./panel/SourceNav";
import { RouteFrame } from "./RouteFrame";
import { AppSidebar } from "./sidebar/AppSidebar";
import { useRefreshSignals } from "./RefreshSignals";
import { TranscriptVisibilityProvider } from "./TranscriptVisibility";
import { useTurnLifecycle } from "./TurnLifecycleSignals";
import { useChatFolderAttachments } from "./useChatFolderAttachments";
import { useDeliverableCatalog } from "./useDeliverableCatalog";
import { backgroundAgentSpawnKeys, useAgentRuns } from "./useAgentRuns";
import { appendTranscript, useVoiceComposer } from "./useVoiceComposer";
import { useVoiceInputStore, voiceSelectionReady } from "./VoiceInputStore";
import { messageWithPastedText, type PastedTextAttachment } from "./PastedText";

const CodeBrowserTab = lazy(async () => {
  const module = await import("./code/browser/CodeBrowserTab");
  return { default: module.CodeBrowserTab };
});

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

function rememberComposerFolder(chatId: string, rootId: string) {
  const current =
    useComposerDrafts.getState().attachments[chatId]?.folders ?? [];
  if (current.includes(rootId)) return;
  composerDraftActions.setFolders(chatId, [...current, rootId]);
}

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
  const { client, models, defaultModelKey, providers, setStatus } = useApp();
  const modelSettingsNav = useModelSettingsNav();
  const { layout, openPanel } = usePanelNav();
  const sourceNav = useStableSourceNav(openPanel);
  const chats = useChatListStore((state) => state.chats);
  const chatsLoaded = useChatListStore((state) => state.chatsLoaded);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const busy = useChatSessionStore((session) => session.busy);
  const lastTurnUsage = useChatSessionStore((session) => session.lastTurnUsage);
  const [hydrated, setHydrated] = useState(false);
  const composerAttachments = useComposerAttachments(chatId);
  const files = composerAttachments.files;
  const pastedTexts = composerAttachments.pastedTexts;
  const pendingFolderIds = composerAttachments.folders;
  const [attaching, setAttaching] = useState(false);
  const [attachError, setAttachError] = useState<string | null>(null);
  const images = useImageAttachments(client, chatId);
  const handleEventRef = useRef<(event: SequencedEvent) => void>(() => {});
  const terminalHydrationGenerationRef = useRef(0);
  // Steering reads the draft synchronously, from outside a render. The route
  // deliberately does not subscribe to the draft — a keystroke re-renders the
  // chat pane (which subscribes in ChatView), not the whole panel arrangement —
  // so the ref is seeded from the store and kept current by setComposerDraft,
  // the only writer on this route.
  const draftRef = useRef(useComposerDrafts.getState().drafts[chatId] ?? "");

  const chat = chats.find((candidate) => candidate.id === chatId) ?? null;
  const nativeHost = hasNativeHost();
  const folders = useChatFolderAttachments(chat, nativeHost);

  // The chat's background work and outputs, observed once here and read by the
  // status chip, the agents table, and the tab strip alike. Subscribed as a
  // joined key rather than the message list: the route must not re-render on
  // every streamed token, and the set of spawn steps changes only when one
  // appears or resolves.
  const spawnKey = useChatSessionStore((session) =>
    backgroundAgentSpawnKeys(session.messages).join(","),
  );
  const spawnKeys = useMemo(
    () => (spawnKey ? spawnKey.split(",") : []),
    [spawnKey],
  );
  const agentRuns = useAgentRuns(client, chatId, spawnKeys);
  const chatAgentRuns = useMemo(
    () =>
      agentRuns.runs.filter(
        (run) =>
          run.tier === "background" &&
          spawnKeys.some((key) => run.id === key || run.spawn_call_id === key),
      ),
    [agentRuns.runs, spawnKeys],
  );
  const deliverables = useDeliverableCatalog(chatId);
  const overlayOpen = usePortalOverlayOpen();
  const [browserTitles, setBrowserTitles] = useState<Record<string, string>>(
    {},
  );

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
    if (pending) {
      if (pending.voiceInputUsed) voice.markInputUsed();
      composerDraftActions.setPastedTexts(chatId, pending.pastedTexts);
      void sendMessage(
        pending.text,
        pending.images,
        pending.files,
        pending.skills,
        pending.voiceInputUsed,
        pending.pastedTexts,
      );
    }
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
        updateSession((session) =>
          sessionFromOpenedChat(session, transcript, pendingApprovals),
        );
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
              text: `Could not load this work: ${String(err)}`,
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
      onMetadata: (metadata) => {
        if (metadata.metadata === "titled") {
          chatListActions.applyDerivedTitle(chatId, metadata.title);
          return;
        }
        if (metadata.metadata === "sandbox_preparing") {
          updateSession((session) => ({
            ...session,
            sandboxPreparing: metadata.preparing,
          }));
          return;
        }
        const generation = ++terminalHydrationGenerationRef.current;
        void refreshTerminalTranscript(generation);
      },
      onConnectionState: (connectionState) =>
        setStatus(
          (current) =>
            `${withoutConnectionState(current)} · ${connectionState}`,
        ),
    });
    controller.start();
    return () => {
      controller.dispose();
      // Leaving the conversation settles its name: coming back to it should show
      // the title, not type it out a second time.
      chatListActions.clearDerivedTitle();
    };
  }, [client, chatId, hydrated, setStatus]);

  function updateSession(
    update: (state: ChatSessionState) => ChatSessionState,
  ) {
    useChatSessionStore.getState().update(update);
  }

  function handleEvent(framed: SequencedEvent) {
    const effects = useChatSessionStore
      .getState()
      .applyEvent(framed, sessionDeps);
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
      case "refresh_task_plan":
        signalRefresh("taskPlan");
        return;
      case "refresh_notifications":
        signalRefresh("notifications");
        return;
      case "warm_presentation_converter":
        warmPresentationConverter();
        return;
      case "turn_began":
        signalTurnLifecycle(
          effect.startsDifferentTurn ? "began" : "began_same_turn",
        );
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

  const voice = useVoiceComposer(
    (audio) => client.transcribeVoice(audio),
    (transcript) =>
      setComposerDraft(appendTranscript(draftRef.current, transcript)),
    undefined,
    async () => {
      const info = await useVoiceInputStore.getState().load(client);
      if (voiceSelectionReady(info)) return true;
      const path: string = "/settings/voice-transcription";
      await navigate({ to: path });
      return false;
    },
  );

  function setComposerFiles(
    update: (current: readonly ImportedDocument[]) => ImportedDocument[],
  ) {
    const current =
      useComposerDrafts.getState().attachments[chatId]?.files ?? [];
    composerDraftActions.setFiles(chatId, update(current));
  }

  async function onSend() {
    await sendMessage(draftRef.current);
  }

  /**
   * Durably queue the draft to run as its own turn once the active one
   * finishes. Same validated body as an ordinary send, parked server-side —
   * the tray above the composer shows and manages the rows.
   */
  async function onQueue() {
    const content = messageWithPastedText(draftRef.current, pastedTexts);
    if (!chat || !content) return;
    const queuedPastedTextIds = new Set(pastedTexts.map((item) => item.id));
    await queueComposerMessage(
      () =>
        client.postMessage(
          chatId,
          crypto.randomUUID(),
          content,
          readyImageAttachmentIds(images.attachments),
          files.map((file) => file.documentId),
          invokedSkills(),
          voice.inputUsed,
          true,
        ),
      () => {
        setComposerDraft("");
        images.clear();
        const current =
          useComposerDrafts.getState().attachments[chatId]?.pastedTexts ?? [];
        composerDraftActions.setPastedTexts(
          chatId,
          current.filter((item) => !queuedPastedTextIds.has(item.id)),
        );
        composerDraftActions.setFolders(chatId, []);
        voice.resetInputUsed();
      },
    );
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
    skillNames: readonly string[] = invokedSkills(),
    voiceInputUsed = voice.inputUsed,
    pastedTextItems: readonly PastedTextAttachment[] = pastedTexts,
  ) {
    const message = messageWithPastedText(content, pastedTextItems);
    await postTurn({
      content: message,
      composerDraft: content.trim(),
      pastedTextIds: pastedTextItems.map((item) => item.id),
      attachments: readyImageAttachmentIds(imageItems),
      transcriptImages: readyTranscriptImageAttachments(imageItems),
      documentIds: fileItems.map((file) => file.documentId),
      transcriptFiles: fileItems.map((file) => ({
        documentId: file.documentId,
        name: file.displayName,
        mediaType: file.mediaType,
      })),
      invokedSkills: skillNames,
      voiceInputUsed,
      fromComposer: true,
    });
  }

  /**
   * The skills the composer is holding, read outside a render: the route does
   * not subscribe to the draft, so a pill picked in the pane below has to be
   * read from the store at the moment the turn is posted.
   */
  function invokedSkills(): readonly string[] {
    return useComposerDrafts.getState().attachments[chatId]?.skills ?? [];
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
      invokedSkills: turn.invokedSkills,
      voiceInputUsed: turn.voiceInputUsed,
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
    composerDraft,
    pastedTextIds = [],
    attachments,
    transcriptImages,
    documentIds,
    transcriptFiles,
    invokedSkills: skillNames,
    voiceInputUsed,
    fromComposer,
  }: {
    content: string;
    composerDraft?: string;
    pastedTextIds?: readonly string[];
    attachments: readonly string[];
    transcriptImages: readonly TranscriptImageAttachment[];
    documentIds: readonly string[];
    transcriptFiles: readonly TranscriptFileAttachment[];
    invokedSkills: readonly string[];
    voiceInputUsed: boolean;
    fromComposer: boolean;
  }) {
    if (!chat || !content || busy || deletingChatId !== null) return;
    const turnId = crypto.randomUUID();
    terminalHydrationGenerationRef.current += 1;
    const optimisticId = nextId();
    const heldImages = fromComposer ? images.attachments : [];
    if (fromComposer) {
      setComposerDraft("");
      images.clear();
    }
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
          voiceInputUsed,
          invokedSkills: [...skillNames],
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
        skillNames,
        voiceInputUsed,
      );
      // Only once the turn is durably accepted, and only for what the composer
      // actually contributed. Images already left the strip with the draft;
      // files, skills, and folders wait so a refused send can still retry.
      if (fromComposer) {
        setComposerFiles(() => []);
        const sentPastedTextIds = new Set(pastedTextIds);
        const current =
          useComposerDrafts.getState().attachments[chatId]?.pastedTexts ?? [];
        composerDraftActions.setPastedTexts(
          chatId,
          current.filter((item) => !sentPastedTextIds.has(item.id)),
        );
        composerDraftActions.setSkills(chatId, []);
        composerDraftActions.setFolders(chatId, []);
        voice.resetInputUsed();
      }
    } catch (err) {
      // Nothing was accepted, so the message has to go back to where it can be
      // fixed and sent again: the text and images return to the composer and
      // the optimistic bubble — which no turn will ever answer — comes out of
      // the transcript.
      if (fromComposer) images.restore(heldImages);
      updateSession((session) => ({
        ...session,
        busy: false,
        activeTurnId: null,
        messages: [
          ...session.messages.filter((message) => message.id !== optimisticId),
          { id: nextId(), role: "error", text: String(err) },
        ],
      }));
      if (!draftRef.current) setComposerDraft(composerDraft ?? content);
      signalTurnLifecycle("resolved");
    }
  }

  /**
   * One picker for anything the reader wants to attach.
   *
   * Which of the two things each file becomes — pixels for the model, or a
   * parsed and readable source — is decided by the host from the bytes, so
   * nothing here has to guess from a name or ask the reader to know first.
   *
   * The host route needs the conversation to be on this computer, because it
   * imports into the store inside this app. A window attached to a machine
   * takes the browser's own picker instead and posts the bytes to the machine
   * that holds the conversation — the same operation, aimed at the right host.
   */
  async function onAttach() {
    if (attaching || deletingChatId !== null) return;
    if (!hasLocalHostAuthority()) {
      await attachHeldFiles();
      return;
    }
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

  /**
   * Attach through the browser, for a window whose conversation is elsewhere.
   *
   * Nothing is marked as attaching until files are actually chosen, so a
   * dismissed picker leaves no spinner to clear. Images take the composer's
   * upload path, which gives them progress and a retry; sources are posted to
   * the machine and adopted through the same code the host route uses.
   */
  async function attachHeldFiles() {
    const chosen = await pickHeldFiles();
    if (chosen.length === 0) return;
    setAttaching(true);
    setAttachError(null);
    const room = Math.max(
      0,
      MAX_IMAGE_ATTACHMENTS - images.attachments.length - files.length,
    );
    const picked = chosen.slice(0, room);
    try {
      const held = await attachHeldChatFiles(client, chatId, picked);
      if (held.images.length > 0) images.attachFiles(held.images);
      adoptAttached({
        images: [],
        documents: held.documents,
        failedImages: [],
      });
      if (picked.length < chosen.length) {
        setAttachError(
          `A message can carry at most ${MAX_IMAGE_ATTACHMENTS} attachments.`,
        );
      }
    } catch (err) {
      setAttachError(friendlyAttachError(err));
    } finally {
      setAttaching(false);
    }
  }

  /**
   * Put a file this conversation already carries back on the next message.
   *
   * Nothing is imported: the document is already in the chat's library, so the
   * message only has to name it again. That is why no size comes with it — the
   * transcript records what a document is, not how many bytes it was.
   */
  /**
   * Open a chat-scoped browser tab.  If one is already in the strip,
   * focus it rather than opening a duplicate.  One default browser tab
   * per chat, seeded with a fresh browser id and a foreground workspace
   * scope the native executor derives identically from the chat id.
   */
  function openBrowser() {
    const existingIndex = layout.tabs.findIndex(
      (tab) => tab.type === "browser",
    );
    if (existingIndex !== -1) {
      openPanel(layout.tabs[existingIndex]);
      return;
    }
    const browserId = crypto.randomUUID();
    const session = seedBrowserSession({
      browserId,
      workspaceId: foregroundBrowserScope(chatId),
    });
    setBrowserTitles((current) => ({
      ...current,
      [browserId]: session.title || "Browser",
    }));
    openPanel({ type: "browser", browserId });
  }

  function onReattachFile(file: TranscriptFileAttachment) {
    if (files.some((current) => current.documentId === file.documentId)) return;
    if (images.attachments.length + files.length >= MAX_IMAGE_ATTACHMENTS) {
      setAttachError(
        `A message can carry at most ${MAX_IMAGE_ATTACHMENTS} attachments.`,
      );
      return;
    }
    setAttachError(null);
    setComposerFiles((current) => [
      ...current,
      {
        documentId: file.documentId,
        displayName: file.name,
        mediaType: file.mediaType,
        byteLen: 0,
      },
    ]);
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
      setAttachError(
        `${failedDocument.displayName}: ${failedDocument.message}`,
      );
    }
    const [failedImage] = attached.failedImages;
    if (failedImage) {
      setAttachError(`${failedImage.fileName}: ${failedImage.message}`);
    }
  }

  async function onModelChange(modelId: ModelSelectionKey | null) {
    if (deletingChatId !== null) return;
    chatListActions.replaceChat(
      await client.patchChatModel(chatId, modelId || null),
    );
  }

  async function onReasoningEffortChange(effort: ReasoningEffort | null) {
    if (deletingChatId !== null) return;
    chatListActions.replaceChat(
      await client.patchChatReasoningEffort(chatId, effort),
    );
  }

  async function onPermissionModeChange(mode: PermissionMode) {
    if (deletingChatId !== null) return;
    chatListActions.replaceChat(
      await client.patchChatPermissionMode(chatId, mode),
    );
  }

  async function onNetworkPolicyChange(policy: NetworkPolicy) {
    if (deletingChatId !== null) return;
    chatListActions.replaceChat(
      await client.patchChatNetworkPolicy(chatId, policy),
    );
  }

  // Shown while the list fetch or the redirect above settles — a blank frame
  // still needs a name so it is not silent to a screen reader.
  if (!chat) {
    return (
      <div
        className="routed-surface-loading"
        role="status"
        aria-label="Loading work"
      />
    );
  }

  function renderChat(visible: boolean) {
    // The model selected right now: the reasoning levels it accepts.
    const activeModel = modelForChat(models, chat!.model, defaultModelKey);
    // Only the levels the selected model accepts are offerable, and a model
    // that accepts none gets no selector at all.
    const efforts = activeModel?.reasoning_efforts ?? [];
    return (
      <TranscriptVisibilityProvider value={visible}>
        <ChatView
          client={client}
          chat={chat!}
          hydrated={hydrated}
          nativeHost={nativeHost}
          deletingChat={deletingChatId !== null}
          attachError={attachError}
          files={{
            items: files,
            attaching,
            onAttach,
            onReattach: onReattachFile,
            onRemove: (documentId) =>
              setComposerFiles((current) =>
                current.filter((file) => file.documentId !== documentId),
              ),
          }}
          folders={{
            items: folders.items,
            pendingIds: pendingFolderIds,
            approved: folders.approved,
            working: folders.working,
            error: folders.error,
            onAttach: nativeHost
              ? () => {
                  void folders.attach().then((connected) => {
                    if (connected)
                      rememberComposerFolder(chatId, connected.rootId);
                  });
                }
              : undefined,
            onConnect: nativeHost
              ? (rootId) => {
                  void folders.connectApproved(rootId).then((connected) => {
                    if (connected)
                      rememberComposerFolder(chatId, connected.rootId);
                  });
                }
              : undefined,
            onRemove: folders.remove,
          }}
          voice={{
            available: voice.available,
            state: voice.state,
            error: voice.error,
            onStart: () => void voice.start(),
            onStop: voice.stop,
          }}
          voiceInputUsed={voice.inputUsed}
          onVoiceInputAccepted={voice.resetInputUsed}
          nativeDropTarget={
            <DocumentDropTarget
              resolveChatId={async () => chatId}
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
            <ModelMenu
              models={models}
              value={chat!.model}
              defaultKey={defaultModelKey}
              disabled={deletingChatId !== null}
              providers={providers}
              onSetUpProvider={modelSettingsNav.onSetUpProvider}
              onChange={onModelChange}
            />
          }
          contextUsage={
            lastTurnUsage
              ? {
                  // Chat has no per-call figure to read, so the ring keeps
                  // the summed turn totals it always used. That carries the
                  // same over-read code mode just fixed: a multi-call turn
                  // re-sends its transcript, so this runs to several prompts
                  // where the window only ever held one. Fixing it needs the
                  // provider paths to publish the last call's prompt on
                  // `RendererTurnUsage`, which is not in this change.
                  contextTokens: summedTurnTokens(lastTurnUsage),
                  spend: {
                    input: lastTurnUsage.input_tokens,
                    output: lastTurnUsage.output_tokens,
                    cacheRead: lastTurnUsage.cache_read_input_tokens,
                    cacheWrite: lastTurnUsage.cache_creation_input_tokens,
                  },
                  contextWindow: activeModel?.context_window,
                  modelName: activeModel?.display_name,
                }
              : null
          }
          composerPermissionMenu={
            <PermissionModeMenu
              scopeKey={chatId}
              value={chat!.permission_mode}
              disabled={deletingChatId !== null}
              onChange={onPermissionModeChange}
            />
          }
          composerReasoning={{
            levels: efforts,
            value: chat!.reasoning_effort,
            disabled: deletingChatId !== null,
            onChange: onReasoningEffortChange,
          }}
          composerNetwork={{
            value: chat!.network_policy,
            disabled: deletingChatId !== null,
            onChange: onNetworkPolicyChange,
          }}
          onDraftChange={(next) => {
            setComposerDraft(next);
            if (!next.trim()) voice.resetInputUsed();
          }}
          onSelectPrompt={(prompt, options) => {
            setComposerDraft(prompt);
            if (options?.enableInternet) {
              void onNetworkPolicyChange({ mode: "open" });
            }
            voice.resetInputUsed();
          }}
          onSend={onSend}
          onQueue={onQueue}
          onRetryTurn={retryTurn}
          onOpenAgentPanel={(runId) => openPanel({ type: "agent", runId })}
          onOpenOutput={(outputId) => openPanel({ type: "outputs", outputId })}
        />
      </TranscriptVisibilityProvider>
    );
  }

  function renderPanel(panel: PanelContent) {
    switch (panel.type) {
      case "document":
        return (
          <DocumentDetailRoot
            chatId={chatId}
            documentID={panel.documentId}
            citationId={panel.citationId}
          />
        );
      case "outputs":
        // An output id turns the list into the reader for that one output.
        return panel.outputId ? (
          <OutputDetailRoot chatId={chatId} outputId={panel.outputId} />
        ) : (
          <PanelFrame spaceBetween>
            <OutputsView
              chatId={chatId}
              onOpen={(outputId) => openPanel({ type: "outputs", outputId })}
            />
          </PanelFrame>
        );
      case "folders":
        return (
          <PanelFrame spaceBetween>
            <FoldersView chat={chat!} />
          </PanelFrame>
        );
      case "permissions":
        return (
          <PanelFrame spaceBetween>
            <PermissionsPanel
              client={client}
              chat={{ id: chat!.id, project_id: chat!.project_id }}
            />
          </PanelFrame>
        );
      case "agents":
        return (
          <AgentsPanel
            runs={chatAgentRuns}
            loading={agentRuns.loading}
            error={agentRuns.error}
            onRetry={agentRuns.refresh}
            onOpenRun={(runId) => openPanel({ type: "agent", runId })}
          />
        );
      case "agent":
        return <BackgroundAgentPanel chatId={chatId} runId={panel.runId} />;
      case "browser":
        return (
          <Suspense fallback={<Skeleton className="h-full w-full" />}>
            <CodeBrowserTab
              workspaceId={foregroundBrowserScope(chatId)}
              browserId={panel.browserId}
              obscured={overlayOpen}
              onTitleChange={(title) =>
                setBrowserTitles((current) =>
                  current[panel.browserId] === title
                    ? current
                    : { ...current, [panel.browserId]: title },
                )
              }
            />
          </Suspense>
        );
      case "terminal":
        return null;
    }
  }

  /**
   * A tab named after the thing it shows, once the route knows the name: an
   * output's filename, an agent's task. Until then the strip's type label
   * stands in.
   */
  function tabLabel(panel: PanelContent): string | undefined {
    switch (panel.type) {
      case "outputs":
        return panel.outputId
          ? deliverables.find((d) => d.outputId === panel.outputId)?.filename
          : undefined;
      case "browser":
        return browserTitles[panel.browserId];
      case "agent":
        return (
          chatAgentRuns.find((run) => run.id === panel.runId)?.task ?? undefined
        );
      default:
        return undefined;
    }
  }

  return (
    <RouteFrame sidebar={<AppSidebar chat={chat} />}>
      <div className="relative mr-2 flex min-h-0 min-w-0 flex-1 flex-col">
        <header className="window-chrome-row mt-2 flex h-9 w-full shrink-0 items-center justify-between gap-2 pr-1">
          <ChatHeaderTitle chat={chat} />
          <div className="relative z-20 flex shrink-0 items-center gap-2 self-start">
            <ChatStatusChip
              compact={layout.tabs.length > 0}
              outputCount={deliverables.length}
              folders={folders.items}
              runs={chatAgentRuns}
              onOpenOutputs={() => openPanel({ type: "outputs" })}
              onOpenFolders={() => openPanel({ type: "folders" })}
              onOpenPermissions={() => openPanel({ type: "permissions" })}
              onOpenAgents={() => openPanel({ type: "agents" })}
              onOpenBrowser={
                hasLocalHostAuthority() ? () => openBrowser() : undefined
              }
            />
          </div>
        </header>
        <PinnedOutputsStrip
          chatId={chatId}
          outputs={deliverables}
          panelOpen={layout.tabs.length > 0}
          onOpenOutput={(outputId) => openPanel({ type: "outputs", outputId })}
          onOpenOutputs={() => openPanel({ type: "outputs" })}
        />
        {/* Citations live in the transcript but open into the panel beside it,
            so the way there is provided above both slots. */}
        <SourceNavProvider value={sourceNav}>
          <PanelLayout
            layout={layout}
            tabLabel={tabLabel}
            renderChat={renderChat}
            renderPanel={renderPanel}
          />
        </SourceNavProvider>
      </div>
    </RouteFrame>
  );
}

function withoutConnectionState(status: string): string {
  return status.replace(/ · (?:live|reconnecting)$/, "");
}

/**
 * Park the composer's message on the server queue, and empty the composer only
 * once the server has it.
 *
 * A refused queue is the one send that has nothing to show for itself: no
 * optimistic bubble goes into the transcript and no turn follows, so a
 * swallowed rejection would take the message away and leave the reader
 * watching a queue that never grew. The text stays where it was typed and the
 * failure is said out loud, the same way the queue tray reports its own.
 */
export async function queueComposerMessage(
  post: () => Promise<unknown>,
  onQueued: () => void,
): Promise<void> {
  try {
    await post();
  } catch (err) {
    toast.error(friendlyErrorMessage(err, "Could not queue that message."));
    return;
  }
  onQueued();
}

function friendlyAttachError(error: unknown): string {
  const message = String(error)
    .replace(/^Error:\s*/, "")
    .trim();
  return message && message.length <= 240
    ? message
    : "Could not attach that file.";
}

function isImportedDocument(result: {
  status: string;
}): result is LibraryImportSuccess {
  return result.status === "imported" || result.status === "already_present";
}
