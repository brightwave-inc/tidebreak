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
  FileText,
  Image as ImageIcon,
  Plus,
  Square,
  Upload,
  X,
} from "lucide-react";
import { MAX_STEER_CHARACTERS } from "./ActiveTurnSteer";
import {
  describeImageAttachment,
  imageFilesFrom,
  imageUploadPercent,
  imageUploadsInFlight,
  transferCarriesFiles,
  type ImageAttachment,
} from "./ImageAttachments";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { WithTooltip } from "@/components/ui/tooltip";

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
  images?: ComposerImages;
  /** Whether the host can open a picker; drop and paste work regardless. */
  canAttach?: boolean;
  attaching?: boolean;
  attachedSourceName?: string | null;
  attachError?: string | null;
  onAttach?: () => Promise<void>;
  onDismissAttachedSource?: () => void;
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
  images,
  canAttach = false,
  attaching = false,
  attachedSourceName = null,
  attachError = null,
  onAttach,
  onDismissAttachedSource,
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
  const [dragging, setDragging] = useState(false);
  const inputDisabled = disabled;
  const active = busy && activeTurnId !== null;
  const hasDraft = Boolean(draft.trim());
  const steerHasUnsupportedCharacter = active && draft.includes("\0");
  const steerTooLong =
    active && [...draft.trim()].length > MAX_STEER_CHARACTERS;
  const imageBlocker = imageSendBlocker(images);
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
    onDraftChange(event.target.value);
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

  return (
    <div className="composer-wrap">
      <form
        className={`composer${dragging ? " is-dropping" : ""}`}
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
        {attachedSourceName && (
          <div className="composer-source" role="status">
            <FileText size={15} aria-hidden="true" />
            <span>
              <strong>{attachedSourceName}</strong>
              <small>Added to this conversation</small>
            </span>
            {onDismissAttachedSource && (
              <button
                type="button"
                aria-label={`Dismiss ${attachedSourceName}`}
                onClick={onDismissAttachedSource}
              >
                <X size={14} aria-hidden="true" />
              </button>
            )}
          </div>
        )}
        {dragging && (
          <div className="composer-drop-hint" role="status">
            <ImageIcon size={15} aria-hidden="true" />
            Drop an image to attach it
          </div>
        )}
        {images && images.items.length > 0 && (
          <ul className="composer-images" aria-label="Attached images">
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
        <textarea
          ref={textareaRef}
          value={draft}
          placeholder={
            active ? "Guide the active response…" : "Message OpenWave…"
          }
          aria-label="Message"
          // A stable hook for the shell's focus-composer shortcut, which has to
          // find the field without a ref threaded up through every route.
          data-composer-input=""
          disabled={inputDisabled}
          onChange={onChange}
          onPaste={onPaste}
          onKeyDown={(event) => {
            if (!shouldSubmitComposerKey(event.nativeEvent)) return;
            event.preventDefault();
            void submit();
          }}
        />
        <div className="composer-actions">
          <div className="composer-actions-left">
            {canAttach && onAttach && (
              <DropdownMenu>
                <WithTooltip label={attaching ? "Attaching…" : "Add"}>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="outline"
                      size="icon-8"
                      aria-label={attaching ? "Attaching" : "Add to this chat"}
                      disabled={inputDisabled || attaching || busy}
                    >
                      <Plus aria-hidden="true" />
                    </Button>
                  </DropdownMenuTrigger>
                </WithTooltip>
                {/* A menu rather than a bare button: attaching files is the
                    first of the things a reader adds to a conversation, and
                    the sources that follow it belong in the same place rather
                    than as a second icon in the row. */}
                <DropdownMenuContent align="start" side="top" className="w-56">
                  <DropdownMenuItem onSelect={() => void onAttach()}>
                    <Upload className="size-4" />
                    Upload files
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            )}
            {modelMenu}
          </div>
          <div className="composer-actions-right">
            {active ? (
              <>
                {(hasDraft || steerPending) && (
                  <button
                    type="submit"
                    className="btn btn-primary composer-redirect"
                    aria-label="Redirect active response"
                    disabled={!canSubmit}
                  >
                    {steerPending ? "Sending…" : "Redirect"}
                  </button>
                )}
                <WithTooltip label={cancelPending ? "Stopping…" : "Stop"}>
                  <button
                    type="button"
                    className="composer-icon-btn composer-stop"
                    aria-label={
                      cancelPending ? "Stopping response" : "Stop response"
                    }
                    disabled={disabled || cancelPending}
                    onClick={() => void onStop()}
                  >
                    <Square size={14} fill="currentColor" strokeWidth={0} />
                  </button>
                </WithTooltip>
              </>
            ) : (
              <WithTooltip label={imageBlocker ?? "Send · Enter"}>
                <button
                  type="submit"
                  className="composer-icon-btn composer-send"
                  aria-label="Send message"
                  disabled={!canSubmit}
                >
                  <ArrowUpRight size={16} />
                </button>
              </WithTooltip>
            )}
          </div>
        </div>
        <span className="sr-only" role="status">
          {busy ? "Agent is responding" : "Ready to send"}
        </span>
        {cancelError && (
          <span className="composer-turn-error" role="status">
            Couldn’t stop turn: {cancelError}
          </span>
        )}
        {steerError && (
          <span className="composer-turn-error" role="alert">
            Couldn’t redirect: {steerError}
          </span>
        )}
        {steerStatus && !steerError && (
          <span className="composer-turn-status" role="status">
            {steerStatus}
          </span>
        )}
        {steerTooLong && (
          <span className="composer-turn-error" role="alert">
            Guidance is too long.
          </span>
        )}
        {steerHasUnsupportedCharacter && (
          <span className="composer-turn-error" role="alert">
            Guidance contains an unsupported character.
          </span>
        )}
        {attachError && (
          <span className="composer-turn-error" role="alert">
            Couldn’t attach: {attachError}
          </span>
        )}
        {images?.error && (
          <span className="composer-turn-error" role="alert">
            Couldn’t attach image: {images.error}
          </span>
        )}
        {images?.unsupportedModel && images.items.length > 0 && (
          <span className="composer-turn-error" role="alert">
            {images.unsupportedModel} can’t read images. Choose a model that
            accepts image input, or remove the attached image.
          </span>
        )}
      </form>
    </div>
  );
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
    <li className={`composer-image is-${attachment.status}`}>
      <span className="composer-image-thumb">
        {attachment.previewUrl ? (
          // Shown from the bytes already in hand rather than fetched back from
          // the server, so the reader sees what they attached immediately.
          <img src={attachment.previewUrl} alt="" />
        ) : (
          <ImageIcon size={16} aria-hidden="true" />
        )}
      </span>
      <span className="composer-image-body">
        <strong title={attachment.name}>{attachment.name}</strong>
        {/* Only the outcome is announced. A live region on the percentage
            would read every tick of a bar that is already on screen. */}
        <small role={failed ? "alert" : uploading ? undefined : "status"}>
          {describeImageAttachment(attachment)}
        </small>
        {uploading && (
          <progress
            className="composer-image-progress"
            max={100}
            value={imageUploadPercent(attachment)}
            aria-label={`Uploading ${attachment.name}`}
          />
        )}
      </span>
      {failed && (
        <button
          type="button"
          className="composer-image-retry"
          onClick={onRetry}
        >
          Try again
        </button>
      )}
      <button
        type="button"
        className="composer-image-remove"
        aria-label={`Remove ${attachment.name}`}
        onClick={onRemove}
      >
        <X size={14} aria-hidden="true" />
      </button>
    </li>
  );
}
