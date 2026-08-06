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
  Wand2,
  X,
} from "lucide-react";
import { MAX_STEER_CHARACTERS } from "./ActiveTurnSteer";
import {
  activeSlashQuery,
  availableSlashOptions,
  filterSlashOptions,
  nextOptionHighlight,
  replaceSlashToken,
  skillsToInvoke,
  MAX_INVOKED_SKILLS,
  type SlashOption,
} from "./ComposerSlash";
import {
  activeMentionQuery,
  attachableFiles,
  attachableFolders,
  mentionOptionRows,
  mentionRows,
  MENTION_LIST_LABEL,
  type MentionAction,
  type MentionCandidate,
  type MentionRow,
} from "./ComposerMentions";
import {
  ComposerToolsMenu,
  type ComposerNetwork,
  type ComposerReasoning,
} from "./ComposerToolsMenu";
import {
  optionIcon,
  pluginOptionRows,
  PluginsPanel,
  PLUGINS_PANEL_LABEL,
  skillCapNote,
} from "@/plugins/PluginsPanel";
import { OptionListbox, optionElementId } from "@/components/OptionListbox";
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
import type { ConnectedFolder } from "./host";
import type { TranscriptFileAttachment } from "./TranscriptFileAttachments";
import type { ChatFolderAccess } from "./useChatFolderAttachments";

const MIN_COMPOSER_LINES = 1;
export const MAX_COMPOSER_LINES = 6;

const SLASH_LIST_ID = "composer-slash-list";
const MENTION_LIST_ID = "composer-mention-list";

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
 * The installed bundles, and how one is turned on.
 *
 * A bundle being on is a property of the installation rather than of this
 * message, so engaging one that is off turns it on the same way its switch on
 * the Plugins page would. What the pick then puts on the message is the
 * composer's business.
 */
export type ComposerPlugins = {
  items: readonly PluginInfo[];
  onSelect: (plugin: PluginInfo) => void;
};

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
  /**
   * Files this conversation already carries, newest first — what `@` offers
   * before the reader has typed anything. Absent on a surface with no
   * transcript to read them from.
   */
  recent?: readonly TranscriptFileAttachment[];
  attaching: boolean;
  onAttach?: () => void;
  /** Put one of `recent` back on the next message. */
  onReattach?: (file: TranscriptFileAttachment) => void;
  onRemove: (documentId: string) => void;
};

export type ComposerFolders = {
  items: ChatFolderAccess[];
  /**
   * Folders approved on this device but not attached here. Approval outlives
   * the chat it was granted in, so these are reachable by name rather than
   * through the picker.
   */
  approved?: readonly ConnectedFolder[];
  working: boolean;
  error: string | null;
  onAttach?: () => void;
  /** Attach one of `approved` to this conversation. */
  onConnect?: (rootId: string) => void;
  onRemove: (rootId: string) => void;
};

export type ComposerVoice = {
  available: boolean;
  state: "idle" | "requesting" | "recording" | "transcribing";
  error: string | null;
  onStart: () => void;
  onStop: () => void;
};

/**
 * What the plugins panel reaches, and which skills the next message will
 * invoke.
 *
 * The composer owns the token, the panel, and its highlight; the surface above
 * it owns the catalog and the invoked names, because those have to survive this
 * component — the chat route reads them again when it posts the turn.
 */
export type ComposerSlash = {
  options: readonly SlashOption[];
  /** Skill names invoked for the next send, in the order they were picked. */
  invoked: readonly string[];
  /**
   * Add these skills to the message, in order. A list rather than one name
   * because a bundle stands for its members and they all arrive at once.
   */
  onInvoke: (names: readonly string[]) => void;
  onRemove: (name: string) => void;
  /** One prompt's insertable text. A rejection leaves the draft as it was. */
  loadPromptBody: (name: string) => Promise<string>;
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
  slash?: ComposerSlash;
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
  slash,
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
  // The `/` token under the caret, as a piece of state rather than a derived
  // value: the caret moves without the draft changing, and a ref would leave
  // the list open over a caret that has walked away from the token.
  const [slashToken, setSlashToken] = useState<{
    start: number;
    query: string;
  } | null>(null);
  const [slashHighlight, setSlashHighlight] = useState(0);
  // The `@` token, kept apart from the `/` one so neither list can inherit the
  // other's highlight or be closed by the other's Escape.
  const [mentionToken, setMentionToken] = useState<{
    start: number;
    query: string;
  } | null>(null);
  const [mentionHighlight, setMentionHighlight] = useState(0);
  // The same list, opened from the tools menu instead of from a `/`. Its query
  // is its own: there is no token in the draft to read one from.
  const [panelQuery, setPanelQuery] = useState<string | null>(null);
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
    setSlashToken(null);
    setMentionToken(null);
  }, [resetKey]);

  const invokedSkills = slash?.invoked ?? [];
  const atSkillCap = invokedSkills.length >= MAX_INVOKED_SKILLS;
  /** What a pick can still reach, given what this message already carries. */
  const slashOptions = availableSlashOptions(slash?.options ?? [], invokedSkills, {
    steering: active,
  });
  const slashMatches = slashToken
    ? filterSlashOptions(slashOptions, slashToken.query)
    : [];
  // Skills vanish from the list at the cap, which would otherwise read as a
  // catalog that has lost them. The note says which bound was reached.
  const capNote = !active && atSkillCap;
  const slashCapNote = slashToken !== null && capNote;
  // An open list with nothing in it is a panel over the draft saying nothing.
  const slashOpen =
    slashToken !== null && (slashMatches.length > 0 || slashCapNote);
  const slashIndex = Math.min(slashHighlight, slashMatches.length - 1);

  /**
   * What `@` can reach: the files this conversation already carries, the
   * folders already approved on this device, and the two pickers behind the
   * tools menu.
   *
   * Every candidate is something the app can already see — a document in this
   * chat's library, a root the broker holds an approval for. Nothing here
   * reaches into the filesystem on its own; a file that has never been given to
   * OpenWave is still reached through the picker, which is what the last rows
   * are for.
   */
  const mentionCandidates: MentionCandidate[] = [
    ...attachableFiles(files?.recent ?? []),
    ...attachableFolders(folders?.approved ?? [], folders?.items ?? []),
  ];
  const mentionActions: MentionAction[] = [
    ...(files?.onAttach ? (["browse-files"] as const) : []),
    ...(folders?.onAttach ? (["connect-folder"] as const) : []),
  ];
  const mentionMatches = mentionToken
    ? mentionRows(mentionCandidates, mentionActions, mentionToken.query)
    : [];
  // The `/` list has the caret when both tokens somehow resolve: it is the
  // older affordance, and two popovers over one draft is one too many.
  const mentionOpen =
    mentionToken !== null && !slashOpen && mentionMatches.length > 0;
  const mentionIndex = Math.min(mentionHighlight, mentionMatches.length - 1);

  /** Re-read whether the caret sits inside a `/` token, and reset the cursor. */
  function syncSlashToken(value: string, caret: number) {
    const token = slash ? activeSlashQuery(value, caret) : null;
    setSlashToken(token);
    setSlashHighlight(0);
    setMentionToken(activeMentionQuery(value, caret));
    setMentionHighlight(0);
  }

  /**
   * Attach what the row named, and take the `@query` out of the draft.
   *
   * The pick goes through the same callbacks the tools menu's own items use, so
   * a mention leaves exactly the chip an attachment leaves — and a refusal
   * (a cap reached, a folder the broker cannot reopen) is reported wherever
   * that path already reports it.
   */
  function pickMention(
    row: MentionRow,
    token: { start: number; query: string },
  ) {
    applySlashReplacement(draft, token.start, token.start + 1 + token.query.length, "");
    if (row.kind === "action") {
      if (row.action === "browse-files") files?.onAttach?.();
      else folders?.onAttach?.();
      return;
    }
    const { candidate } = row;
    if (candidate.kind === "folder") {
      folders?.onConnect?.(candidate.id);
      return;
    }
    const file = files?.recent?.find(
      (recent) => recent.documentId === candidate.id,
    );
    if (file) files?.onReattach?.(file);
  }

  /** The `@` list's keys. Only reached when the `/` list did not claim them. */
  function handleMentionKey(event: KeyboardEvent<HTMLTextAreaElement>): boolean {
    if (event.key === "Escape") {
      if (mentionToken === null) return false;
      setMentionToken(null);
      return true;
    }
    if (!mentionOpen) return false;
    const moved = nextOptionHighlight(
      event.key,
      mentionIndex,
      mentionMatches.length,
    );
    if (moved !== null) {
      setMentionHighlight(moved);
      return true;
    }
    if (event.key === "Enter" || event.key === "Tab") {
      const row = mentionMatches[mentionIndex];
      if (row && mentionToken) {
        setMentionToken(null);
        pickMention(row, mentionToken);
      }
      return true;
    }
    return false;
  }

  /**
   * Do what the row promised, and take the `/query` out of the draft when the
   * pick came from one.
   *
   * A skill leaves a pill behind: the name travels beside the message rather
   * than inside it, so nothing has to be parsed back out of prose. A bundle
   * leaves one per member skill, and turns itself on first if it was off. A
   * prompt is text, so its body goes into the draft — fetched only now, because
   * the catalog deliberately carries no bodies.
   */
  function pickOption(
    option: SlashOption,
    token: { start: number; query: string } | null,
  ) {
    if (!slash) return;
    const caret = token ? token.start + 1 + token.query.length : 0;
    if (option.kind === "prompt") {
      void (async () => {
        let body: string;
        try {
          body = await slash.loadPromptBody(option.name);
        } catch {
          // Picking is not a place to raise an error: a body that cannot be
          // read leaves the draft exactly as the reader typed it, token and all.
          return;
        }
        if (token) applySlashReplacement(draft, token.start, caret, body);
        else insertAtSelection(body);
      })();
      return;
    }
    if (token) applySlashReplacement(draft, token.start, caret, "");
    if (option.kind === "plugin") {
      const plugin = plugins?.items.find((item) => item.name === option.name);
      if (plugin) plugins?.onSelect(plugin);
    }
    const names = skillsToInvoke(option, invokedSkills);
    if (names.length > 0) slash.onInvoke(names);
  }

  function applySlashReplacement(
    source: string,
    start: number,
    caret: number,
    replacement: string,
  ) {
    const next = replaceSlashToken(source, start, caret, replacement);
    moveCaret(next.text, next.caret);
  }

  /** Text put in where the reader last had the caret, without running words together. */
  function insertAtSelection(text: string) {
    const remembered = selectionRef.current;
    const start = Math.min(remembered?.start ?? draft.length, draft.length);
    const end = Math.min(remembered?.end ?? draft.length, draft.length);
    const before = draft.slice(0, start);
    const gap = before && !/\s$/.test(before) ? " " : "";
    moveCaret(
      `${before}${gap}${text}${draft.slice(end)}`,
      before.length + gap.length + text.length,
    );
  }

  function moveCaret(text: string, caret: number) {
    selectionRef.current = { start: caret, end: caret };
    onDraftChange(text);
    // After the panel or menu has closed and the new value has been painted;
    // focusing while either is still up hands focus straight back to it.
    window.requestAnimationFrame(() => {
      const field = textareaRef.current;
      if (!field || field.disabled) return;
      field.focus();
      field.setSelectionRange(caret, caret);
    });
  }

  /** The list's own keys, taken before the textarea's send-on-Enter sees them. */
  function handleSlashKey(event: KeyboardEvent<HTMLTextAreaElement>): boolean {
    if (event.key === "Escape") {
      // Only claimed when there is a list to close, so Escape keeps whatever
      // meaning the surrounding surface gives it the rest of the time.
      if (slashToken === null) return false;
      setSlashToken(null);
      return true;
    }
    if (!slashOpen) return false;
    // A list showing only the cap note has nothing to move through or pick, so
    // Enter stays what it always is: send.
    if (slashMatches.length === 0) return false;
    const moved = nextOptionHighlight(event.key, slashIndex, slashMatches.length);
    if (moved !== null) {
      setSlashHighlight(moved);
      return true;
    }
    if (event.key === "Enter" || event.key === "Tab") {
      const option = slashMatches[slashIndex];
      if (option) {
        setSlashToken(null);
        pickOption(option, slashToken);
      }
      return true;
    }
    return false;
  }

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
    syncSlashToken(event.target.value, event.target.selectionStart);
    onDraftChange(event.target.value);
  }

  /** Where the reader last had the caret, for text inserted from a menu. */
  function rememberSelection(textarea: HTMLTextAreaElement) {
    selectionRef.current = {
      start: textarea.selectionStart,
      end: textarea.selectionEnd,
    };
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
      {slash && invokedSkills.length > 0 && (
        <ul
          className="m-0 flex list-none flex-wrap gap-2 p-0"
          aria-label="Invoked skills"
        >
          {invokedSkills.map((name) => (
            <InvokedSkillChip
              key={name}
              option={skillOption(slash.options, name)}
              name={name}
              // Steering carries no invocation, so a chip is set aside for as
              // long as a turn is running rather than silently applying later.
              inactive={active}
              onRemove={() => slash.onRemove(name)}
            />
          ))}
        </ul>
      )}
      {panelQuery !== null && slash && (
        <PluginsPanel
          options={slashOptions}
          query={panelQuery}
          capNote={capNote}
          onQueryChange={setPanelQuery}
          onPick={(option) => {
            setPanelQuery(null);
            pickOption(option, null);
          }}
          onClose={() => setPanelQuery(null)}
        />
      )}
      {slashOpen && (
        // In flow above the field rather than floating over it: the composer
        // clips its own overflow to keep its rounded edge, so a panel anchored
        // above the box would be cut off at the border.
        <div className="rounded-md border border-border bg-popover text-popover-foreground shadow-md">
          <OptionListbox
            listId={SLASH_LIST_ID}
            label={PLUGINS_PANEL_LABEL}
            rows={pluginOptionRows(slashMatches)}
            activeIndex={slashIndex}
            note={slashCapNote ? skillCapNote() : null}
            onPick={(index) => {
              const option = slashMatches[index];
              if (!option) return;
              setSlashToken(null);
              pickOption(option, slashToken);
            }}
            onHighlight={setSlashHighlight}
          />
        </div>
      )}
      {mentionOpen && (
        <div className="rounded-md border border-border bg-popover text-popover-foreground shadow-md">
          <OptionListbox
            listId={MENTION_LIST_ID}
            label={MENTION_LIST_LABEL}
            rows={mentionOptionRows(mentionMatches)}
            activeIndex={mentionIndex}
            onPick={(index) => {
              const row = mentionMatches[index];
              if (!row || !mentionToken) return;
              const token = mentionToken;
              setMentionToken(null);
              pickMention(row, token);
            }}
            onHighlight={setMentionHighlight}
          />
        </div>
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
        aria-controls={
          slashOpen ? SLASH_LIST_ID : mentionOpen ? MENTION_LIST_ID : undefined
        }
        aria-activedescendant={
          slashOpen
            ? optionElementId(SLASH_LIST_ID, slashIndex)
            : mentionOpen
              ? optionElementId(MENTION_LIST_ID, mentionIndex)
              : undefined
        }
        // A stable hook for the shell's focus-composer shortcut, which has to
        // find the field without a ref threaded up through every route.
        data-composer-input=""
        disabled={inputDisabled}
        onChange={onChange}
        onSelect={(event) => {
          rememberSelection(event.currentTarget);
          syncSlashToken(
            event.currentTarget.value,
            event.currentTarget.selectionStart,
          );
        }}
        onBlur={() => {
          setSlashToken(null);
          setMentionToken(null);
        }}
        onPaste={onPaste}
        onKeyDown={(event) => {
          if (handleSlashKey(event) || handleMentionKey(event)) {
            event.preventDefault();
            return;
          }
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
              slash && slash.options.length > 0
                ? {
                    onOpen: () => {
                      setSlashToken(null);
                      setMentionToken(null);
                      setPanelQuery("");
                    },
                  }
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

/** What the catalog calls a skill, or nothing if the catalog has lost it. */
function skillOption(
  options: readonly SlashOption[],
  name: string,
): SlashOption | undefined {
  return options.find(
    (candidate) => candidate.kind === "skill" && candidate.name === name,
  );
}

/**
 * One skill this message will invoke.
 *
 * A pill rather than words in the draft: the invocation is a field on the
 * message, so it has to be visible and removable as one thing, not as text the
 * reader could half-delete. It carries the icon of the library it came from,
 * which is what makes the several a bundle leaves behind read as one pick.
 */
function InvokedSkillChip({
  option,
  name,
  inactive,
  onRemove,
}: {
  option: SlashOption | undefined;
  name: string;
  inactive: boolean;
  onRemove: () => void;
}) {
  const label = option?.label ?? name;
  const Icon = option ? optionIcon(option) : Wand2;
  return (
    <li
      className={cn(
        "relative flex min-w-0 max-w-full items-center gap-2 rounded-full border border-border bg-muted/50 py-1 pl-2 pr-7 text-muted-foreground",
        inactive && "opacity-60",
      )}
    >
      <span className="inline-flex size-6 shrink-0 items-center justify-center rounded-full bg-background">
        <Icon size={14} aria-hidden="true" />
      </span>
      <span
        className="max-w-[12rem] truncate text-xs font-semibold text-foreground"
        title={label}
      >
        {label}
      </span>
      <small className="text-[0.68rem]">
        {inactive ? "Next message" : "Skill"}
      </small>
      <button
        type="button"
        className="absolute right-1 top-1/2 inline-flex -translate-y-1/2 items-center justify-center rounded-full border-0 bg-transparent p-0.5 text-inherit hover:bg-accent hover:text-foreground"
        aria-label={`Remove ${label}`}
        onClick={onRemove}
      >
        <X size={13} aria-hidden="true" />
      </button>
    </li>
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
        {/* A file put back on the message by name carries its identity, not its
            size — the transcript records what a document is, not how big it
            was. An unknown size is left unsaid rather than reported as zero. */}
        {file.byteLen > 0 && (
          <small className="text-[0.68rem]">{formatBytes(file.byteLen)}</small>
        )}
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
