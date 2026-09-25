import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { Folder, X } from "lucide-react";

import { useApp } from "./AppContext";
import {
  attachChatFiles,
  attachHeldChatFiles,
  pickHeldFiles,
} from "./attachments";
import { useChatListStore } from "./ChatListStore";
import { Composer, type ComposerImages } from "./Composer";
import {
  homeDraftKey,
  useComposerAttachments,
  useComposerDraft,
  useComposerDrafts,
} from "./ComposerDrafts";
import { type ImportedDocument, type LibraryImportSuccess } from "./documents";
import { DocumentDropTarget } from "./DocumentDropTarget";
import { useFirstMessage } from "./FirstMessage";
import { hasLocalHostAuthority } from "./host";
import { useManagedPolicy } from "./managedPolicy";
import { ModelMenu, useModelSettingsNav } from "./ModelMenu";
import { modelForSelection, textOnlyModelLabel } from "./ModelSelection";
import {
  noModelSendBlocker,
  noRunnableModel,
  openGatewaySettings,
  openProviderSetup,
} from "./modelSetup";
import {
  effectiveNewChatSettings,
  useNewChatSettings,
} from "./NewChatSettings";
import {
  PermissionModeMenu,
  WORK_PERMISSION_MODE_DESCRIPTIONS,
} from "./PermissionModeMenu";
import { pluginsApisFromClient } from "./plugins/pluginsApis";
import { useProjectListStore } from "./ProjectListStore";
import { ManagedModelNotice, ProviderSetupCard } from "./ProviderSetupCard";
import { useComposerPlugins } from "./plugins/useComposerPlugins";
import { WelcomeState } from "./WelcomeState";
import { PaneDragBand } from "./WindowDragStrip";
import type { AttachedFiles } from "./attachments";
import { MAX_IMAGE_ATTACHMENTS } from "./ImageAttachments";
import { useImageAttachments } from "./useImageAttachments";
import { appendTranscript, useVoiceComposer } from "./useVoiceComposer";
import { useVoiceInputStore, voiceSelectionReady } from "./VoiceInputStore";
import { messageWithPastedText } from "./PastedText";
import {
  FirstTaskWalkthrough,
  shouldOfferFirstTaskWalkthrough,
} from "./FirstTaskWalkthrough";
import { Button } from "@/components/ui/button";
import { Notice } from "@/components/ui/notice";
import { friendlyErrorMessage } from "./lib/utils";

const chatListActions = useChatListStore.getState();
const composerDraftActions = useComposerDrafts.getState();
const firstMessageActions = useFirstMessage.getState();

/**
 * A file picker creates a chat before there is a first message. Reconcile the
 * home pickers immediately before that first message is held so changes made
 * while the attachment strip was open govern the turn that follows.
 *
 * That includes the model and its reasoning level: the chat was created with
 * whatever was selected when the attachment opened it, and a reader who picks
 * a different model before sending expects the turn to run on the one they can
 * see in the composer.
 *
 * A null model is not sent, and the reasoning level rides with it. It means
 * the home picker never resolved one — loading the defaults is allowed to fail
 * quietly — and the chat the server seeded from the sticky default is a better
 * answer than the global default that clearing it would fall back to.
 */
export async function applyPendingChatSettings(
  client: Pick<
    typeof import("./api").ApiClient.prototype,
    | "patchChatModel"
    | "patchChatReasoningEffort"
    | "patchChatPermissionMode"
    | "patchChatNetworkPolicy"
  >,
  chatId: string,
  settings: {
    model: import("./api").ModelSelectionKey | null;
    reasoningEffort: import("./api").ReasoningEffort | null;
    permissionMode: import("./api").PermissionMode | null;
    networkPolicy: import("./api").NetworkPolicy;
  },
): Promise<void> {
  if (settings.model) {
    chatListActions.replaceChat(
      await client.patchChatModel(chatId, settings.model),
    );
    chatListActions.replaceChat(
      await client.patchChatReasoningEffort(chatId, settings.reasoningEffort),
    );
  }
  chatListActions.replaceChat(
    await client.patchChatPermissionMode(chatId, settings.permissionMode),
  );
  chatListActions.replaceChat(
    await client.patchChatNetworkPolicy(chatId, settings.networkPolicy),
  );
}

/**
 * One in-flight run shared by everyone who asks for it while it is running.
 *
 * Home creates its pending chat lazily, and the callers race: a dropped batch
 * of three images asks for the chat three times in the same tick, and the
 * paperclip can be mid-flight when a paste lands. Without this each caller
 * creates its own chat and the reader is left with orphans in the sidebar.
 * The run is forgotten as soon as it settles, so a failed creation is retried
 * rather than remembered.
 */
export function singleFlight<T>(): (run: () => Promise<T>) => Promise<T> {
  let inFlight: Promise<T> | null = null;
  return (run) => {
    if (!inFlight) {
      inFlight = run().finally(() => {
        inFlight = null;
      });
    }
    return inFlight;
  };
}

function isImportedDocument(result: {
  status: string;
}): result is LibraryImportSuccess {
  return result.status === "imported" || result.status === "already_present";
}

/**
 * Where new work starts: the composer alone, and a conversation only once the
 * first message goes.
 *
 * `projectId` starts the work inside a project. The conversation is filed
 * there when it is created, and the draft is that project's own, so leaving to
 * look something up and coming back through the project finds it again.
 */
export function HomeRoute({
  projectId = null,
}: {
  projectId?: string | null;
} = {}) {
  const navigate = useNavigate();
  const { client, models, defaultModelKey, providers, catalogLoaded } =
    useApp();
  const { managed } = useManagedPolicy();
  const modelSettingsNav = useModelSettingsNav();
  // Nothing can run until a provider is connected. A starter or a message
  // sent now could only fail, so home offers the ways to connect instead.
  const noModelCanRun = noRunnableModel(models, catalogLoaded);
  const sendBlocker = useMemo(
    () => noModelSendBlocker(noModelCanRun, managed, navigate),
    [noModelCanRun, managed, navigate],
  );
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const project = useProjectListStore((state) =>
    projectId
      ? state.projects.find((candidate) => candidate.id === projectId)
      : undefined,
  );
  const projectsLoaded = useProjectListStore((state) => state.projectsLoaded);
  const draftKey = homeDraftKey(projectId);
  const draft = useComposerDraft(draftKey);
  const composerPlugins = useComposerPlugins(client);
  const promptLibrary = useMemo(() => pluginsApisFromClient(client), [client]);
  const setDraft = (text: string) =>
    composerDraftActions.setDraft(draftKey, text);
  const voice = useVoiceComposer(
    (audio) => client.transcribeVoice(audio),
    (transcript) => {
      const current = useComposerDrafts.getState().drafts[draftKey] ?? "";
      setDraft(appendTranscript(current, transcript));
    },
    undefined,
    async () => {
      const info = await useVoiceInputStore.getState().load(client);
      if (voiceSelectionReady(info)) return true;
      const path: string = "/settings/voice-transcription";
      await navigate({ to: path });
      return false;
    },
  );
  const [error, setError] = useState<string | null>(null);
  const [walkthroughAvailable, setWalkthroughAvailable] = useState(
    shouldOfferFirstTaskWalkthrough,
  );
  const [walkthroughOpen, setWalkthroughOpen] = useState(false);
  const newChat = useNewChatSettings();
  // What the pickers show and the created chat will get: this visit's picks
  // over the server's sticky defaults. Only the explicit picks are sent; the
  // server seeds the rest from the same defaults being displayed.
  const effective = effectiveNewChatSettings(newChat);
  const efforts =
    modelForSelection(models, effective.model)?.reasoning_efforts ?? [];

  // A choice made inside a chat is recorded server-side as the sticky
  // default; re-read it whenever the reader lands back here so the pickers
  // show what the next chat will actually start with.
  useEffect(() => {
    void useNewChatSettings.getState().loadDefaults(client);
  }, [client]);

  // A chat created silently when the user attaches files before typing. The
  // chat exists on the server so files can upload, but the user stays on the
  // home page until they send. The id is part of the home draft: without it a
  // restored attachment strip would publish to a chat nobody remembers.
  const attachments = useComposerAttachments(draftKey);
  const pendingChatId = attachments.pendingChatId;
  const pendingImages = attachments.images;
  const pendingFiles = attachments.files;
  const pendingPastedTexts = attachments.pastedTexts;
  const pendingSkills = attachments.skills;
  const [attaching, setAttaching] = useState(false);
  const [attachError, setAttachError] = useState<string | null>(null);
  // One chat creation shared by every route into it — the picker, and each file
  // of a dropped or pasted batch.
  const creatingPendingChat = useRef(singleFlight<string>());
  // The same strip a conversation's composer has. Home's bytes are published
  // into the chat the attachment silently creates, which is why the target is
  // resolved per upload rather than being the draft's own key.
  const images = useImageAttachments(client, draftKey, ensurePendingChat);

  const chats = useChatListStore((state) => state.chats);
  const chatsLoaded = useChatListStore((state) => state.chatsLoaded);

  // A project that is gone — deleted in another window, or a stale link —
  // cannot hold new work, so the composer starts outside it instead.
  useEffect(() => {
    if (projectId && projectsLoaded && !project) {
      void navigate({ to: "/", replace: true });
    }
  }, [projectId, projectsLoaded, project, navigate]);

  // A restored home draft may point at a chat that no longer exists — deleted
  // in another window since the attachments were published to it. Those images
  // and files can never send, so drop them; the text stands on its own.
  useEffect(() => {
    if (!chatsLoaded || !pendingChatId) return;
    if (chats.some((chat) => chat.id === pendingChatId)) return;
    composerDraftActions.setPendingChatId(draftKey, null);
    composerDraftActions.setImages(draftKey, []);
    composerDraftActions.setFiles(draftKey, []);
  }, [chatsLoaded, chats, pendingChatId]);

  function setPendingFiles(
    update: (current: readonly ImportedDocument[]) => ImportedDocument[],
  ) {
    const current =
      useComposerDrafts.getState().attachments[draftKey]?.files ?? [];
    composerDraftActions.setFiles(draftKey, update(current));
  }

  async function ensurePendingChat(): Promise<string> {
    const existing =
      useComposerDrafts.getState().attachments[draftKey]?.pendingChatId;
    if (existing) return existing;
    // Images arrive in batches — a multi-file drop uploads every file at once,
    // and each upload asks for the chat to publish into. One in-flight creation
    // is shared between them so a three-image drop does not leave two orphan
    // chats behind it.
    return creatingPendingChat.current(createPendingChat);
  }

  async function createPendingChat(): Promise<string> {
    const created = await client.createChat(
      newChat.model ?? undefined,
      projectId,
      {
        reasoningEffort: newChat.reasoningEffort,
        permissionMode: newChat.permissionMode,
        networkPolicy: newChat.networkPolicy ?? undefined,
      },
    );
    chatListActions.prependChat(created);
    chatListActions.setChatsError(null);
    composerDraftActions.setPendingChatId(draftKey, created.id);
    return created.id;
  }

  /**
   * The host picker imports into the store inside this app, which is not where
   * the conversation lives while this window is attached to a machine. That
   * window takes the browser's own picker and posts the bytes to the machine
   * instead, so the attach succeeds rather than failing once files are chosen.
   */
  async function onAttach() {
    if (attaching || creatingChat) return;
    if (!hasLocalHostAuthority()) {
      await attachHeldFiles();
      return;
    }
    setAttaching(true);
    setAttachError(null);
    try {
      const chatId = await ensurePendingChat();
      const attached = await attachChatFiles(chatId);
      if (!attached) return;
      adoptAttached(attached);
    } catch (err) {
      setAttachError(friendlyErrorMessage(err, "Could not attach that file."));
    } finally {
      setAttaching(false);
    }
  }

  /**
   * Nothing is marked as attaching until files are chosen, so a dismissed
   * picker leaves no spinner behind — and no conversation is created for a
   * selection the reader abandoned.
   */
  async function attachHeldFiles() {
    await attachChosenHeldFiles(await pickHeldFiles());
  }

  /**
   * Route files the renderer already holds — picker, drop, or paste — the same
   * way the hosted picker does. Creating the pending chat is part of ingesting
   * a source; images still upload through the composer once that chat exists.
   */
  async function attachChosenHeldFiles(chosen: readonly File[]) {
    if (chosen.length === 0) return;
    setAttaching(true);
    setAttachError(null);
    const room = Math.max(
      0,
      MAX_IMAGE_ATTACHMENTS - pendingImages.length - pendingFiles.length,
    );
    const picked = chosen.slice(0, room);
    try {
      const chatId = await ensurePendingChat();
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
      setAttachError(friendlyErrorMessage(err, "Could not attach that file."));
    } finally {
      setAttaching(false);
    }
  }

  function adoptAttached(attached: AttachedFiles) {
    const seenDocumentIds = new Set(
      pendingFiles.map((file) => file.documentId),
    );
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
      MAX_IMAGE_ATTACHMENTS - pendingImages.length - pendingFiles.length;
    const pickedImages = attached.images.slice(0, Math.max(0, remaining));
    const pickedFiles = imported.slice(
      0,
      Math.max(0, remaining - pickedImages.length),
    );
    if (pickedImages.length > 0) {
      images.adopt(pickedImages);
    }
    if (pickedFiles.length > 0) {
      setPendingFiles((current) => [...current, ...pickedFiles]);
    }
    if (
      pickedImages.length + pickedFiles.length <
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

  async function startChat() {
    const content = messageWithPastedText(draft, pendingPastedTexts);
    if (!content || creatingChat) return;
    chatListActions.setCreatingChat(true);
    setError(null);
    try {
      // Reuse the chat that was silently created for file attachments, or
      // create a fresh one.
      let chatId = pendingChatId;
      if (!chatId) {
        const created = await client.createChat(
          newChat.model ?? undefined,
          projectId,
          {
            reasoningEffort: newChat.reasoningEffort,
            permissionMode: newChat.permissionMode,
            networkPolicy: newChat.networkPolicy ?? undefined,
          },
        );
        chatListActions.prependChat(created);
        chatListActions.setChatsError(null);
        chatId = created.id;
      } else {
        await applyPendingChatSettings(client, chatId, {
          model: effective.model,
          reasoningEffort: effective.reasoningEffort,
          permissionMode: effective.permissionMode,
          networkPolicy: effective.networkPolicy,
        });
      }
      firstMessageActions.hold(chatId, {
        text: draft.trim(),
        images: pendingImages,
        files: pendingFiles,
        pastedTexts: pendingPastedTexts,
        skills: pendingSkills,
        voiceInputUsed: voice.inputUsed,
      });
      // Clear the home draft only once navigation has committed. If it throws,
      // the message lives only in the FirstMessage store with no composer
      // showing it — the draft has to stay where the reader can see and
      // resend it.
      if (projectId) {
        useProjectListStore.getState().expandProject(projectId);
        await navigate({
          to: "/p/$projectId/c/$chatId",
          params: { projectId, chatId },
        });
      } else {
        await navigate({ to: "/c/$chatId", params: { chatId } });
      }
      composerDraftActions.clearDraft(draftKey);
    } catch (err) {
      setError(
        `Could not start a conversation: ${friendlyErrorMessage(err, "Try again.")}`,
      );
    } finally {
      chatListActions.setCreatingChat(false);
    }
  }

  // Offered whether or not anything is attached yet: a drop or a paste is how
  // the first image usually arrives, and the composer only claims one when a
  // strip is there to receive it.
  const composerImages: ComposerImages = {
    items: pendingImages,
    error: images.error,
    unsupportedModel: textOnlyModelLabel(models, effective.model),
    onAttachFiles: (selected) => {
      if (
        pendingImages.length + pendingFiles.length + selected.length >
        MAX_IMAGE_ATTACHMENTS
      ) {
        setAttachError(
          `A message can carry at most ${MAX_IMAGE_ATTACHMENTS} attachments.`,
        );
        return;
      }
      setAttachError(null);
      images.attachFiles(selected);
    },
    onRemove: images.remove,
    onRetry: images.retry,
  };

  // Home is the composer alone. The install-wide libraries that used to open
  // as panels here are routes of their own now, so nothing beside the
  // conversation starter needs hosting.
  return (
    <>
      <div className="content-container flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden">
        {/* The panel slot this used to sit in was a plain block, so nothing
            stretches the column to the slot's height — it has to claim it
            itself, the same way .chat-pane does. */}
        <div className="flex h-full min-h-0 w-full min-w-0 flex-col overflow-hidden px-[clamp(0.5rem,4%,5rem)]">
          <h1 className="sr-only" tabIndex={-1}>
            Home
          </h1>
          <div className="relative flex min-h-0 flex-1 justify-center overflow-y-auto">
            <PaneDragBand />
            {/* The same null state an empty conversation shows: home is where a
              chat starts, so it greets the same way. Picking a starter prompt
              fills the composer rather than sending, the way it does in a chat.
              Web starters also turn internet access on so the turn can search
              without another click. Home's starters come from the installed
              prompt library when it has any; otherwise the built-in openers
              stand. */}
            <div className="my-auto w-full">
              <WelcomeState
                onSelectPrompt={(prompt, options) => {
                  setDraft(prompt);
                  if (options?.enableInternet) {
                    newChat.setNetworkPolicy({ mode: "open" });
                  }
                  voice.resetInputUsed();
                }}
                executionConfigClient={client}
                promptLibrary={promptLibrary}
                heading={
                  noModelCanRun
                    ? managed
                      ? "No models are available yet"
                      : "Connect a model to start"
                    : walkthroughAvailable
                      ? "Welcome to Tidebreak"
                      : undefined
                }
                description={
                  noModelCanRun
                    ? managed
                      ? "Your organization's gateway provides the models here."
                      : "Tidebreak runs on models you bring. Pick one way to connect. You can add more later in Settings."
                    : walkthroughAvailable
                      ? "Choose how the agent works, add what it needs, and start with a real task."
                      : undefined
                }
                onStartWalkthrough={
                  walkthroughAvailable && !walkthroughOpen
                    ? () => setWalkthroughOpen(true)
                    : undefined
                }
                setup={
                  noModelCanRun ? (
                    managed ? (
                      <ManagedModelNotice
                        onOpenGateway={() => openGatewaySettings(navigate)}
                      />
                    ) : (
                      <ProviderSetupCard
                        onSetUp={(target) =>
                          openProviderSetup(navigate, target)
                        }
                      />
                    )
                  ) : undefined
                }
              />
            </div>
          </div>

          <div className="z-10 mx-auto w-full max-w-3xl pb-2">
            {project && (
              <div className="flex min-w-0 items-center gap-1.5 pt-1 pb-1.5 text-sm text-muted-foreground">
                <Folder aria-hidden="true" className="size-3.5 shrink-0" />
                {/* The project's name is what truncates, so a narrow pane
                    still says where the work goes. */}
                <span className="shrink-0">New conversation in</span>
                <span className="min-w-0 truncate font-medium text-foreground">
                  {project.title?.trim() || "Untitled project"}
                </span>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  className="shrink-0"
                  aria-label="Start outside the project"
                  title="Start outside the project"
                  disabled={creatingChat}
                  onClick={() => void navigate({ to: "/" })}
                >
                  <X />
                </Button>
              </div>
            )}
            {error && (
              <Notice tone="critical" className="mb-2">
                {error}
              </Notice>
            )}
            <Composer
              activeTurnId={null}
              busy={false}
              cancelError={null}
              cancelPending={false}
              disabled={creatingChat}
              draft={draft}
              plugins={composerPlugins.plugins}
              slash={{
                options: composerPlugins.slashOptions,
                invoked: pendingSkills,
                onInvoke: (names) =>
                  composerDraftActions.setSkills(draftKey, [
                    ...pendingSkills,
                    ...names,
                  ]),
                onRemove: (name) =>
                  composerDraftActions.setSkills(
                    draftKey,
                    pendingSkills.filter((skill) => skill !== name),
                  ),
                loadPromptBody: composerPlugins.loadPromptBody,
              }}
              images={composerImages}
              voice={{
                available: voice.available,
                state: voice.state,
                error: voice.error,
                onStart: () => void voice.start(),
                onStop: voice.stop,
              }}
              files={{
                items: pendingFiles,
                attaching,
                onAttach,
                onAttachHeld: (chosen) => {
                  void attachChosenHeldFiles(chosen);
                },
                onRemove: (documentId) =>
                  setPendingFiles((current) =>
                    current.filter((file) => file.documentId !== documentId),
                  ),
              }}
              pastedTexts={{
                items: pendingPastedTexts,
                onPaste: (text) => {
                  const current =
                    useComposerDrafts.getState().attachments[draftKey]
                      ?.pastedTexts ?? [];
                  composerDraftActions.setPastedTexts(draftKey, [
                    ...current,
                    { id: crypto.randomUUID(), text },
                  ]);
                },
                onRemove: (id) => {
                  const current =
                    useComposerDrafts.getState().attachments[draftKey]
                      ?.pastedTexts ?? [];
                  composerDraftActions.setPastedTexts(
                    draftKey,
                    current.filter((item) => item.id !== id),
                  );
                },
              }}
              nativeDropTarget={
                <DocumentDropTarget
                  resolveChatId={ensurePendingChat}
                  onAttached={adoptAttached}
                  onError={(caught) =>
                    setAttachError(
                      friendlyErrorMessage(
                        caught,
                        "Could not attach that file.",
                      ),
                    )
                  }
                />
              }
              attachError={attachError}
              resetKey={draftKey}
              steerError={null}
              steerPending={false}
              steerStatus={null}
              modelMenu={
                <ModelMenu
                  models={models}
                  value={effective.model}
                  defaultKey={defaultModelKey}
                  lastUsed={
                    newChat.model === null && Boolean(newChat.defaults?.model)
                  }
                  disabled={creatingChat}
                  providers={providers}
                  onSetUpProvider={modelSettingsNav.onSetUpProvider}
                  onChange={newChat.setModel}
                />
              }
              permissionMenu={
                <PermissionModeMenu
                  scopeKey="new-chat"
                  value={effective.permissionMode}
                  disabled={creatingChat}
                  descriptions={WORK_PERMISSION_MODE_DESCRIPTIONS}
                  onChange={newChat.setPermissionMode}
                />
              }
              reasoning={{
                levels: efforts,
                value: effective.reasoningEffort,
                disabled: creatingChat,
                onChange: newChat.setReasoningEffort,
              }}
              network={{
                value: effective.networkPolicy,
                disabled: creatingChat,
                onChange: newChat.setNetworkPolicy,
              }}
              onDraftChange={(next) => {
                setDraft(next);
                if (!next.trim()) voice.resetInputUsed();
              }}
              sendBlocker={sendBlocker}
              onSend={startChat}
              onSteer={async () => {}}
              onStop={async () => {}}
            />
          </div>
        </div>
      </div>
      <FirstTaskWalkthrough
        open={walkthroughOpen}
        onClose={() => {
          setWalkthroughOpen(false);
          setWalkthroughAvailable(false);
        }}
      />
    </>
  );
}
