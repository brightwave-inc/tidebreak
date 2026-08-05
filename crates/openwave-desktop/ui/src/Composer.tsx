import {
  type ChangeEvent,
  type ClipboardEvent,
  type DragEvent,
  type KeyboardEvent,
  type ReactNode,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import {
  ArrowUpRight,
  FolderOpen,
  Image as ImageIcon,
  Mic,
  Square,
  X,
} from "lucide-react";
import { MAX_STEER_CHARACTERS } from "./ActiveTurnSteer";
import {
  ComposerToolsMenu,
  type ComposerNetwork,
  type ComposerPlugins,
  type ComposerReasoning,
} from "./ComposerToolsMenu";
import {
  describeImageAttachment,
  imageFilesFrom,
  imageUploadPercent,
  imageUploadsInFlight,
  transferCarriesFiles,
  type ImageAttachment,
} from "./ImageAttachments";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ConfirmDialog";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { documentIcon } from "@/documentIcon";
import type { ImportedDocument } from "@/documents";
import type { PluginInfo } from "./api";
import { folderAccessLabel, folderReach } from "./FolderAccess";
import type { ChatFolderAccess } from "./useChatFolderAttachments";

const MIN_COMPOSER_LINES = 1;
export const MAX_COMPOSER_LINES = 6;

type ComposerKeyEvent = Pick<
  KeyboardEvent<HTMLTextAreaElement>["nativeEvent"],
  | "altKey"
  | "ctrlKey"
  | "isComposing"
  | "key"
  | "keyCode"
  | "metaKey"
  | "shiftKey"
>;

export function shouldSubmitComposerKey(event: ComposerKeyEvent): boolean {
  return (
    event.key === "Enter" &&
    !event.shiftKey &&
    !event.ctrlKey &&
    !event.altKey &&
    !event.metaKey &&
    !event.isComposing &&
    event.keyCode !== 229
  );
}

export function shouldRestoreComposerFocus(
  submissionKey: string,
  currentKey: string,
  inputDisabled: boolean,
): boolean {
  return submissionKey === currentKey && !inputDisabled;
}

export function boundedComposerHeight(
  scrollHeight: number,
  lineHeight: number,
  verticalInsets: number,
): number {
  const minimum = lineHeight * MIN_COMPOSER_LINES + verticalInsets;
  const maximum = lineHeight * MAX_COMPOSER_LINES + verticalInsets;
  return Math.max(minimum, Math.min(scrollHeight, maximum));
}

function resizeComposerTextarea(textarea: HTMLTextAreaElement): void {
  const styles = window.getComputedStyle(textarea);
  const lineHeight = Number.parseFloat(styles.lineHeight) || 20;
  const verticalInsets =
    (Number.parseFloat(styles.paddingTop) || 0) +
    (Number.parseFloat(styles.paddingBottom) || 0) +
    (Number.parseFloat(styles.borderTopWidth) || 0) +
    (Number.parseFloat(styles.borderBottomWidth) || 0);
  const maximum = lineHeight * MAX_COMPOSER_LINES + verticalInsets;

  textarea.style.height = "auto";
  textarea.style.height = `${boundedComposerHeight(
    textarea.scrollHeight,
    lineHeight,
    verticalInsets,
  )}px`;
  textarea.style.overflowY = textarea.scrollHeight > maximum ? "auto" : "hidden";
}

/**
 * The sentence that engages a plugin, and where the caret lands after it.
 *
 * There is no directive the backend parses: an enabled bundle's skills are in
 * the model's system prompt, so asking for one in plain words is what makes it
 * happen. The text goes in at the caret, with a space in front of it when it
 * would otherwise run into what is already written.
 */
export function insertPluginDirective(
  draft: string,
  directive: string,
  selectionStart: number,
  selectionEnd: number,
): { text: string; caret: number } {
  const before = draft.slice(0, selectionStart);
  const after = draft.slice(selectionEnd);
  const gap = before && !/\s$/.test(before) ? " " : "";
  return {
    text: `${before}${gap}${directive}${after}`,
    caret: before.length + gap.length + directive.length,
  };
}

export function pluginDirective(displayName: string): string {
  return `Use the ${displayName} plugin: `;
}

/**
 * Everything the composer needs to present attached images.
 *
 * Grouped rather than spread across the prop list because these move together:
 * a surface either offers image attachment or it does not.
 */
export type ComposerImages = {
  items: ImageAttachment[];
  error: string | null;
  /**
   * The selected model's label when it cannot read images, so the composer can
   * say so before the send that would be refused.
   */
  unsupportedModel: string | null;
  onAttachFiles: (files: readonly File[]) => void;
  onRemove: (id: string) => void;
  onRetry: (id: string) => void;
};

export type ComposerFiles = {
  items: ImportedDocument[];
  attaching: boolean;
  onAttach?: () => void;
  onRemove: (documentId: string) => void;
};

export type ComposerFolders = {
  items: ChatFolderAccess[];
  working: boolean;
  error: string | null;
  onAttach?: () => void;
  onRemove: (rootId: string) => void;
};

export type ComposerVoice = {
  available: boolean;
  state: "idle" | "requesting" | "recording" | "transcribing";
  error: string | null;
  onStart: () => void;
  onStop: () => void;
};

/** Whether attached images stop this turn from being sent, and why. */
export function imageSendBlocker(images: ComposerImages | undefined): string | null {
  if (!images || images.items.length === 0) return null;
  if (imageUploadsInFlight(images.items)) return "Waiting for images to upload";
  if (images.items.some((item) => item.status === "failed")) {
    return "An image did not upload";
  }
  if (images.unsupportedModel) {
    return `${images.unsupportedModel} cannot read images`;
  }
  return null;
}

export type ComposerProps = {
  activeTurnId: string | null;
  busy: boolean;
  cancelError: string | null;
  cancelPending: boolean;
  disabled: boolean;
  draft: string;
  modelMenu?: ReactNode;
  permissionMenu?: ReactNode;
  network?: ComposerNetwork;
  reasoning?: ComposerReasoning;
  plugins?: ComposerPlugins;
  images?: ComposerImages;
  files?: ComposerFiles;
  folders?: ComposerFolders;
  voice?: ComposerVoice;
  nativeDropTarget?: ReactNode;
  attachError?: string | null;
  onDraftChange: (draft: string) => void;
  onSend: () => Promise<void>;
  onSteer: () => Promise<void>;
  onStop: () => Promise<void>;
  resetKey: string;
  steerError: string | null;
  steerPending: boolean;
  steerStatus: string | null;
};

export function Composer({
  activeTurnId,
  busy,
  cancelError,
  cancelPending,
  disabled,
  draft,
  modelMenu,
  permissionMenu,
  network,
  reasoning,
  plugins,
  images,
  files,
  folders,
  voice,
  nativeDropTarget,
  attachError = null,
  onDraftChange,
  onSend,
  onSteer,
  onStop,
  resetKey,
  steerError,
  steerPending,
  steerStatus,
}: ComposerProps) {
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const latestResetKeyRef = useRef(resetKey);
  latestResetKeyRef.current = resetKey;
  // dragenter and dragleave fire for every descendant the pointer crosses, so a
  // boolean flickers the drop hint on and off while the file is held still.
  const dragDepthRef = useRef(0);
  const selectionRef = useRef<{ start: number; end: number } | null>(null);
  const [dragging, setDragging] = useState(false);
  const { confirm, dialog: confirmDialog } = useConfirm();
  const inputDisabled = disabled;
  const active = busy && activeTurnId !== null;
  const hasDraft = Boolean(draft.trim());
  const steerHasUnsupportedCharacter = active && draft.includes("\0");
  const steerTooLong =
    active && [...draft.trim()].length > MAX_STEER_CHARACTERS;
  const imageBlocker = imageSendBlocker(images);
  const voiceWorking = voice?.state !== undefined && voice.state !== "idle";
  const canSubmit =
    !inputDisabled &&
    !steerPending &&
    !cancelPending &&
    hasDraft &&
    !steerHasUnsupportedCharacter &&
    !steerTooLong &&
    imageBlocker === null &&
    (!busy || active);

  useLayoutEffect(() => {
    const textarea = textareaRef.current;
    if (textarea) resizeComposerTextarea(textarea);
  }, [draft, resetKey]);

  // A different conversation is a different draft: the caret from the last one
  // means nothing in it.
  useLayoutEffect(() => {
    selectionRef.current = null;
  }, [resetKey]);

  async function submit(): Promise<void> {
    if (!canSubmit) return;
    const submissionKey = resetKey;
    await (active ? onSteer() : onSend());

    // Restore focus after accepted guidance or a failed request. A new chat or
    // disabled composer must never receive focus from an older submission.
    window.requestAnimationFrame(() => {
      const textarea = textareaRef.current;
      if (
        textarea &&
        shouldRestoreComposerFocus(
          submissionKey,
          latestResetKeyRef.current,
          textarea.disabled,
        )
      ) {
        textarea.focus();
      }
    });
  }

  function onChange(event: ChangeEvent<HTMLTextAreaElement>) {
    rememberSelection(event.target);
    onDraftChange(event.target.value);
  }

  /** Where the reader last had the caret, for text inserted from a menu. */
  function rememberSelection(textarea: HTMLTextAreaElement) {
    selectionRef.current = {
      start: textarea.selectionStart,
      end: textarea.selectionEnd,
    };
  }

  /**
   * Turn the bundle on, then say so in the message being written.
   *
   * The caret is restored after the insertion so the reader carries on typing
   * their request where the sentence left off, rather than at the end of a
   * draft they may have been editing in the middle of. A draft nobody has put
   * a caret in yet — a starter prompt, a transcript — is appended to.
   */
  function engagePlugin(plugin: PluginInfo) {
    plugins?.onSelect(plugin);
    const remembered = selectionRef.current;
    const start = Math.min(remembered?.start ?? draft.length, draft.length);
    const end = Math.min(remembered?.end ?? draft.length, draft.length);
    const { text, caret } = insertPluginDirective(
      draft,
      pluginDirective(plugin.display_name),
      start,
      end,
    );
    selectionRef.current = { start: caret, end: caret };
    onDraftChange(text);
    // After the menu has closed and the new value has been painted; focusing
    // while the menu is still up hands focus straight back to the trigger.
    window.requestAnimationFrame(() => {
      const field = textareaRef.current;
      if (!field || field.disabled) return;
      field.focus();
      field.setSelectionRange(caret, caret);
    });
  }

  function endDrag() {
    dragDepthRef.current = 0;
    setDragging(false);
  }

  /**
   * Images from a drop or a paste take the same route as the picker's, so the
   * chip, the progress, and the failure behave identically whichever way the
   * reader chose. A paste that carries no image is left alone: it is text.
   */
  function acceptTransfer(transfer: DataTransfer | null): boolean {
    if (!images || inputDisabled) return false;
    const files = imageFilesFrom(transfer);
    if (files.length === 0) return false;
    images.onAttachFiles(files);
    return true;
  }

  function onDrop(event: DragEvent<HTMLFormElement>) {
    endDrag();
    // The webview, not the host, receives file drops (`dragDropEnabled: false`),
    // and its own handling of one is to navigate away from the app and display
    // the file — so a drop must be claimed here whether or not it is taken.
    event.preventDefault();
    acceptTransfer(event.dataTransfer);
  }

  function onPaste(event: ClipboardEvent<HTMLTextAreaElement>) {
    if (acceptTransfer(event.clipboardData)) event.preventDefault();
  }

  async function removeFolder(folder: ChatFolderAccess) {
    const accepted = await confirm({
      title: `Disconnect ${folder.displayName}?`,
      description: "The agent loses access to this folder.",
      confirmLabel: "Disconnect",
      destructive: true,
    });
    if (accepted) folders?.onRemove(folder.rootId);
  }

  return (
    <form
      className={cn(
        "relative mx-auto flex w-full max-w-3xl flex-col gap-1 overflow-hidden rounded-xl border border-border bg-background p-4 shadow-sm transition-colors focus-within:border-ring focus-within:shadow-lg",
        dragging && "border-primary shadow-lg",
      )}
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
      onDragEnter={(event) => {
        if (!images || inputDisabled) return;
        if (!transferCarriesFiles(event.dataTransfer)) return;
        dragDepthRef.current += 1;
        setDragging(true);
      }}
      onDragOver={(event) => {
        if (dragDepthRef.current > 0) event.preventDefault();
      }}
      onDragLeave={() => {
        dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
        if (dragDepthRef.current === 0) setDragging(false);
      }}
      onDrop={onDrop}
    >
      {nativeDropTarget}
      {dragging && (
        <div
          className="flex items-center gap-2 rounded-lg border border-dashed border-border px-3 py-2 text-xs text-muted-foreground"
          role="status"
        >
          <ImageIcon size={15} aria-hidden="true" />
          Drop an image to attach it
        </div>
      )}
      {images && images.items.length > 0 && (
        <ul
          className="m-0 flex list-none flex-wrap gap-2 p-0"
          aria-label="Attached images"
        >
          {images.items.map((item) => (
            <ImageAttachmentChip
              key={item.id}
              attachment={item}
              onRemove={() => images.onRemove(item.id)}
              onRetry={() => images.onRetry(item.id)}
            />
          ))}
        </ul>
      )}
      {files && files.items.length > 0 && (
        <ul
          className="m-0 flex list-none flex-wrap gap-2 p-0"
          aria-label="Attached files"
        >
          {files.items.map((file) => (
            <FileAttachmentChip
              key={file.documentId}
              file={file}
              onRemove={() => files.onRemove(file.documentId)}
            />
          ))}
        </ul>
      )}
      {folders && folders.items.length > 0 && (
        <ul
          className="m-0 flex list-none flex-wrap gap-2 p-0"
          aria-label="Attached folders"
        >
          {folders.items.map((folder) => (
            <FolderAttachmentChip
              key={folder.rootId}
              folder={folder}
              disabled={folders.working}
              onRemove={() => void removeFolder(folder)}
            />
          ))}
        </ul>
      )}
      <textarea
        ref={textareaRef}
        className={cn(
          "w-full resize-none border-none bg-transparent px-1 text-base placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-0",
          voiceWorking && "placeholder:italic",
        )}
        value={draft}
        placeholder={
          voice?.state === "requesting" || voice?.state === "recording"
            ? "Listening…"
            : voice?.state === "transcribing"
              ? "Transcribing…"
              : active
                ? "Guide the active response…"
                : "Message OpenWave…"
        }
        aria-label="Message"
        // A stable hook for the shell's focus-composer shortcut, which has to
        // find the field without a ref threaded up through every route.
        data-composer-input=""
        disabled={inputDisabled}
        onChange={onChange}
        onSelect={(event) => rememberSelection(event.currentTarget)}
        onPaste={onPaste}
        onKeyDown={(event) => {
          if (!shouldSubmitComposerKey(event.nativeEvent)) return;
          event.preventDefault();
          void submit();
        }}
      />
      <div className="flex items-center justify-between gap-2">
        <div className="flex grow items-center gap-2">
          <ComposerToolsMenu
            disabled={inputDisabled}
            attachFiles={
              files?.onAttach
                ? { attaching: files.attaching, onAttach: files.onAttach }
                : undefined
            }
            attachFolder={
              folders?.onAttach
                ? { working: folders.working, onAttach: folders.onAttach }
                : undefined
            }
            network={network}
            reasoning={reasoning}
            plugins={
              plugins
                ? { items: plugins.items, onSelect: engagePlugin }
                : undefined
            }
          />
          {modelMenu}
        </div>
        <div className="flex items-center gap-2">
          {/* The permission mode sits with the send cluster: it is what the
              next turn will be allowed to do, not another way to prepare it. */}
          {permissionMenu}
          {/* The mic sits immediately before send: both act on the draft, so
              they belong in the same cluster, apart from the tools and
              model controls that only set the turn up. */}
          {voice?.available && (
            <WithTooltip
              label={
                voice.state === "recording"
                  ? "Stop recording"
                  : voice.state === "transcribing"
                      ? "Transcribing…"
                      : "Record voice input"
              }
            >
              <Button
                type="button"
                variant={voice.state === "idle" ? "outline" : "default"}
                size="icon-8"
                className={cn(
                  "shrink-0",
                  voice.state !== "idle" &&
                    "bg-foreground text-background hover:bg-foreground/90",
                )}
                aria-label={
                  voice.state === "recording"
                    ? "Stop voice recording"
                    : voice.state === "transcribing"
                      ? "Transcribing voice recording"
                      : voice.state === "requesting"
                        ? "Waiting for microphone permission"
                        : "Record voice message"
                }
                disabled={
                  inputDisabled ||
                  voice.state === "transcribing"
                }
                onClick={
                  voice.state === "recording" ? voice.onStop : voice.onStart
                }
              >
                <Mic size={15} />
              </Button>
            </WithTooltip>
          )}
          {active ? (
            <>
              {(hasDraft || steerPending) && (
                <Button
                  type="submit"
                  variant="default"
                  size="sm"
                  className="min-w-[5rem]"
                  aria-label="Redirect active response"
                  disabled={!canSubmit}
                >
                  {steerPending ? "Sending…" : "Redirect"}
                </Button>
              )}
              <WithTooltip label={cancelPending ? "Stopping…" : "Stop"}>
                <Button
                  type="button"
                  variant="default"
                  size="icon-8"
                  aria-label={
                    cancelPending ? "Stopping response" : "Stop response"
                  }
                  disabled={disabled || cancelPending}
                  onClick={() => void onStop()}
                >
                  <Square size={14} fill="currentColor" strokeWidth={0} />
                </Button>
              </WithTooltip>
            </>
          ) : (
            <WithTooltip label={imageBlocker ?? "Send · Enter"}>
              <Button
                type="submit"
                variant="default"
                size="icon-8"
                aria-label="Send message"
                disabled={!canSubmit}
              >
                <ArrowUpRight size={16} />
              </Button>
            </WithTooltip>
          )}
        </div>
      </div>
      <span className="sr-only" role="status">
        {voiceWorking
          ? voice?.state === "transcribing"
            ? "Transcribing voice recording"
            : "Listening for voice input"
          : busy
            ? "Agent is responding"
            : "Ready to send"}
      </span>
      {voice?.error && (
        <span className="text-xs text-destructive" role="alert">
          {voice.error}
        </span>
      )}
      {cancelError && (
        <span className="text-xs text-destructive" role="status">
          {"Couldn’t stop turn: "}{cancelError}
        </span>
      )}
      {steerError && (
        <span className="text-xs text-destructive" role="alert">
          {"Couldn’t redirect: "}{steerError}
        </span>
      )}
      {steerStatus && !steerError && (
        <span className="text-xs text-muted-foreground" role="status">
          {steerStatus}
        </span>
      )}
      {steerTooLong && (
        <span className="text-xs text-destructive" role="alert">
          Guidance is too long.
        </span>
      )}
      {steerHasUnsupportedCharacter && (
        <span className="text-xs text-destructive" role="alert">
          Guidance contains an unsupported character.
        </span>
      )}
      {attachError && (
        <span className="text-xs text-destructive" role="alert">
          {"Couldn’t attach: "}{attachError}
        </span>
      )}
      {folders?.error && (
        <span className="text-xs text-destructive" role="alert">
          {"Couldn’t update folders: "}{folders.error}
        </span>
      )}
      {images?.error && (
        <span className="text-xs text-destructive" role="alert">
          {"Couldn’t attach image: "}{images.error}
        </span>
      )}
      {images?.unsupportedModel && images.items.length > 0 && (
        <span className="text-xs text-destructive" role="alert">
          {images.unsupportedModel}
          {" can’t read images. Choose a model that accepts image input, or remove the attached image."}
        </span>
      )}
      {confirmDialog}
    </form>
  );
}

function FolderAttachmentChip({
  folder,
  disabled,
  onRemove,
}: {
  folder: ChatFolderAccess;
  disabled: boolean;
  onRemove: () => void;
}) {
  return (
    <li className="relative flex min-w-0 max-w-full items-center gap-2 rounded-lg border border-border bg-muted/50 py-1.5 pl-2 pr-7 text-muted-foreground">
      <span className="inline-flex size-9 shrink-0 items-center justify-center rounded-md bg-background">
        <FolderOpen size={16} aria-hidden="true" />
      </span>
      <span className="grid min-w-0 gap-px">
        <strong
          className="max-w-[12rem] truncate text-xs font-semibold text-foreground"
          title={folder.displayName}
        >
          {folder.displayName}
        </strong>
        <small className="text-[0.68rem]">
          {folderAccessLabel(folderReach(folder.statements))}
        </small>
      </span>
      <button
        type="button"
        className="absolute right-0.5 top-0.5 inline-flex items-center justify-center rounded-full border-0 bg-transparent p-0.5 text-inherit hover:bg-accent hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
        aria-label={`Disconnect ${folder.displayName}`}
        disabled={disabled}
        onClick={onRemove}
      >
        <X size={14} aria-hidden="true" />
      </button>
    </li>
  );
}

function FileAttachmentChip({
  file,
  onRemove,
}: {
  file: ImportedDocument;
  onRemove: () => void;
}) {
  const Icon = documentIcon(file.mediaType);
  return (
    <li className="relative flex min-w-0 max-w-full items-center gap-2 rounded-lg border border-border bg-muted/50 py-1.5 pl-2 pr-7 text-muted-foreground">
      <span className="inline-flex size-9 shrink-0 items-center justify-center rounded-md bg-background">
        <Icon size={16} aria-hidden="true" />
      </span>
      <span className="grid min-w-0 gap-px">
        <strong
          className="max-w-[12rem] truncate text-xs font-semibold text-foreground"
          title={file.displayName}
        >
          {file.displayName}
        </strong>
        <small className="text-[0.68rem]">{formatBytes(file.byteLen)}</small>
      </span>
      <button
        type="button"
        className="absolute right-0.5 top-0.5 inline-flex items-center justify-center rounded-full border-0 bg-transparent p-0.5 text-inherit hover:bg-accent hover:text-foreground"
        aria-label={`Remove ${file.displayName}`}
        onClick={onRemove}
      >
        <X size={14} aria-hidden="true" />
      </button>
    </li>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_024 * 1_024) return `${Math.ceil(bytes / 1_024)} KB`;
  return `${(bytes / (1_024 * 1_024)).toFixed(1)} MB`;
}

/**
 * One attached image, from the moment it is attached to the moment it is sent.
 *
 * A failed chip stays put and offers another attempt. Removing it silently
 * would leave the reader believing the image went with their message; making
 * them attach it again would throw away a file the composer is still holding.
 */
function ImageAttachmentChip({
  attachment,
  onRemove,
  onRetry,
}: {
  attachment: ImageAttachment;
  onRemove: () => void;
  onRetry: () => void;
}) {
  const uploading =
    attachment.status === "queued" || attachment.status === "uploading";
  const failed = attachment.status === "failed";
  return (
    <li
      className={cn(
        "relative flex min-w-0 max-w-full items-center gap-2 rounded-lg border border-border bg-muted/50 py-1.5 pl-2 pr-7 text-muted-foreground",
        failed && "border-destructive",
      )}
    >
      <span className="inline-flex size-9 shrink-0 items-center justify-center overflow-hidden rounded-md bg-background">
        {attachment.previewUrl ? (
          // Shown from the bytes already in hand rather than fetched back from
          // the server, so the reader sees what they attached immediately.
          <img className="size-full object-cover" src={attachment.previewUrl} alt="" />
        ) : (
          <ImageIcon size={16} aria-hidden="true" />
        )}
      </span>
      <span className="grid min-w-0 gap-px">
        <strong className="max-w-[12rem] truncate text-xs font-semibold text-foreground" title={attachment.name}>
          {attachment.name}
        </strong>
        {/* Only the outcome is announced. A live region on the percentage
            would read every tick of a bar that is already on screen. */}
        <small className={cn("text-[0.68rem]", failed && "text-destructive")} role={failed ? "alert" : uploading ? undefined : "status"}>
          {describeImageAttachment(attachment)}
        </small>
        {uploading && (
          <progress
            className="mt-0.5 h-[3px] w-full appearance-none rounded-full border-0 bg-border [&::-moz-progress-bar]:rounded-full [&::-moz-progress-bar]:bg-foreground [&::-webkit-progress-bar]:rounded-full [&::-webkit-progress-bar]:bg-border [&::-webkit-progress-value]:rounded-full [&::-webkit-progress-value]:bg-foreground [&::-webkit-progress-value]:transition-[width_120ms_linear]"
            max={100}
            value={imageUploadPercent(attachment)}
            aria-label={`Uploading ${attachment.name}`}
          />
        )}
      </span>
      {failed && (
        <button
          type="button"
          className="shrink-0 rounded-full border border-border bg-background px-2 py-px text-[0.68rem] text-foreground"
          onClick={onRetry}
        >
          Try again
        </button>
      )}
      <button
        type="button"
        className="absolute right-0.5 top-0.5 inline-flex items-center justify-center rounded-full border-0 bg-transparent p-0.5 text-inherit hover:bg-accent hover:text-foreground"
        aria-label={`Remove ${attachment.name}`}
        onClick={onRemove}
      >
        <X size={14} aria-hidden="true" />
      </button>
    </li>
  );
}
