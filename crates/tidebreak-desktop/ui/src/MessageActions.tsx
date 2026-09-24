import { useState, type KeyboardEvent, type ReactNode } from "react";
import {
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  GitBranch,
  Pencil,
  RefreshCw,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Textarea } from "@/components/ui/textarea";
import { WithTooltip } from "@/components/ui/tooltip";
import type { TurnSideEffect } from "./api";

/** One provider's models a retry can pick, in menu order. */
export type RetryModelGroup = {
  label: string;
  models: readonly { key: string; label: string }[];
};

/**
 * What the reader can do to a settled turn in a Work chat.
 *
 * Absent for a transcript that only shows history. Every action goes through
 * the server, so a turn another client reran shows the same result here after
 * the next refresh.
 */
export type TurnActions = {
  /** Answer the latest message again, with `model` or the chat's model. */
  onRegenerate: (turnId: string, model?: string) => void;
  /** Replace the latest message with `text` and answer it. */
  onEdit: (turnId: string, text: string) => void;
  /** Start a new chat with a copy of the history through `turnId`. */
  onBranch: (turnId: string) => void;
  /** The models "Retry with model" offers. Empty hides the menu. */
  retryModels: readonly RetryModelGroup[];
  /** The model the chat runs, marked in the menu. */
  currentModelKey: string | null;
  /** Whether a rerun or a branch is being sent right now. */
  pending: boolean;
};

/** One icon-only action under a message, labelled for every reader. */
export function MessageActionButton({
  label,
  onClick,
  disabled,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <WithTooltip label={label}>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        className="message-action"
        aria-label={label}
        disabled={disabled}
        onClick={onClick}
      >
        {children}
      </Button>
    </WithTooltip>
  );
}

/**
 * Said under an earlier answer while it is on screen: the conversation goes
 * on from the latest answer, not the one shown.
 */
export function AnswerVersionNote({ latest }: { latest: number }) {
  return (
    <p className="message-versions-note">
      The conversation continues from answer {latest}.
    </p>
  );
}

/** "2 of 3", with a step to each side, for an answer given more than once. */
export function AnswerVersionPager({
  index,
  count,
  onSelect,
}: {
  /** Zero-based position of the version on screen. */
  index: number;
  count: number;
  onSelect: (index: number) => void;
}) {
  return (
    <div className="message-versions" role="group" aria-label="Answer versions">
      <MessageActionButton
        label="Previous version"
        disabled={index <= 0}
        onClick={() => onSelect(index - 1)}
      >
        <ChevronLeft aria-hidden="true" />
      </MessageActionButton>
      <span className="message-versions-label" aria-live="polite">
        {index + 1} of {count}
      </span>
      <MessageActionButton
        label="Next version"
        disabled={index >= count - 1}
        onClick={() => onSelect(index + 1)}
      >
        <ChevronRight aria-hidden="true" />
      </MessageActionButton>
    </div>
  );
}

/**
 * Regenerate, with "Retry with model" beside it when there is a model to
 * pick. The answer on screen becomes an earlier version either way.
 *
 * When answering again would start a new chat, because the answer on screen
 * acted outside this one, Regenerate opens a menu that says so before
 * anything is sent, the way the editor does for an edit.
 */
export function RegenerateControl({
  onRegenerate,
  retryModels,
  currentModelKey,
  disabled,
  newChatNote = null,
}: {
  onRegenerate: (model?: string) => void;
  retryModels: readonly RetryModelGroup[];
  currentModelKey: string | null;
  disabled: boolean;
  /** Why answering again starts a new chat, or `null` when it does not. */
  newChatNote?: string | null;
}) {
  const [open, setOpen] = useState(false);
  const confirms = newChatNote !== null;
  const regenerate = (
    <MessageActionButton
      label="Regenerate"
      disabled={disabled}
      onClick={() => (confirms ? setOpen(true) : onRegenerate())}
    >
      <RefreshCw aria-hidden="true" />
    </MessageActionButton>
  );
  if (!confirms && retryModels.length === 0) {
    return <span className="message-action-pair">{regenerate}</span>;
  }
  return (
    <span className="message-action-pair">
      <DropdownMenu open={open} onOpenChange={setOpen}>
        {retryModels.length > 0 ? (
          <>
            {regenerate}
            <WithTooltip label="Retry with model…">
              <DropdownMenuTrigger asChild>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  // Narrow and joined to Regenerate, so the pair reads as one
                  // control with a choice attached.
                  className="message-action h-6 w-4 [&_svg]:size-3"
                  aria-label="Retry with model…"
                  disabled={disabled}
                >
                  <ChevronDown aria-hidden="true" />
                </Button>
              </DropdownMenuTrigger>
            </WithTooltip>
          </>
        ) : (
          <WithTooltip label="Regenerate">
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                className="message-action"
                aria-label="Regenerate"
                disabled={disabled}
              >
                <RefreshCw aria-hidden="true" />
              </Button>
            </DropdownMenuTrigger>
          </WithTooltip>
        )}
        <DropdownMenuContent
          align="start"
          className="message-retry-models max-h-80 overflow-y-auto"
        >
          {confirms && (
            <>
              <p className="message-rerun-note">{newChatNote}</p>
              <DropdownMenuItem onSelect={() => onRegenerate()}>
                <RefreshCw aria-hidden="true" />
                Regenerate in new chat
              </DropdownMenuItem>
            </>
          )}
          {retryModels.length > 0 && confirms && <DropdownMenuSeparator />}
          {retryModels.length > 0 && (
            <p className="px-2 pt-1 pb-1 text-xs font-medium text-muted-foreground">
              Retry with model
            </p>
          )}
          {retryModels.map((group, groupIndex) => (
            <DropdownMenuGroup key={group.label} aria-label={group.label}>
              {groupIndex > 0 && <DropdownMenuSeparator />}
              <p className="px-2 pt-1.5 pb-0.5 text-xs text-muted-foreground">
                {group.label}
              </p>
              {group.models.map((model) => (
                <DropdownMenuItem
                  key={model.key}
                  onSelect={() => onRegenerate(model.key)}
                >
                  <span className="min-w-0 flex-1 truncate">{model.label}</span>
                  {model.key === currentModelKey && (
                    <Check
                      aria-label="The chat's model"
                      className="text-muted-foreground"
                    />
                  )}
                </DropdownMenuItem>
              ))}
            </DropdownMenuGroup>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
    </span>
  );
}

export function BranchButton({
  onBranch,
  disabled,
}: {
  onBranch: () => void;
  disabled: boolean;
}) {
  return (
    <MessageActionButton
      label="Branch from here"
      disabled={disabled}
      onClick={onBranch}
    >
      <GitBranch aria-hidden="true" />
    </MessageActionButton>
  );
}

export function EditButton({
  onEdit,
  disabled,
}: {
  onEdit: () => void;
  disabled: boolean;
}) {
  return (
    <MessageActionButton label="Edit" disabled={disabled} onClick={onEdit}>
      <Pencil aria-hidden="true" />
    </MessageActionButton>
  );
}

/** What an answer did outside the chat, as one phrase: "wrote files and ran commands". */
function sideEffectsPhrase(effects: readonly TurnSideEffect[]): string {
  const done = effects.map(
    (effect) =>
      ({
        files_written: "wrote files",
        connected_apps_called: "used connected apps",
        commands_run: "ran commands",
        other_actions: "took other actions",
      })[effect],
  );
  return done.length === 1
    ? done[0]
    : `${done.slice(0, -1).join(", ")} and ${done[done.length - 1]}`;
}

/**
 * The sentence an edit shows when it will start a new chat, or `null` when it
 * replaces the message in place.
 */
export function editStartsNewChatCopy(
  effects: readonly TurnSideEffect[],
): string | null {
  if (effects.length === 0) return null;
  return `Your edit replaces an answer that ${sideEffectsPhrase(effects)}, so it starts a new chat. This chat stays as it is.`;
}

/**
 * The sentence Regenerate shows when answering again will start a new chat,
 * or `null` when it answers in place.
 */
export function regenerateStartsNewChatCopy(
  effects: readonly TurnSideEffect[],
): string | null {
  if (effects.length === 0) return null;
  return `This answer ${sideEffectsPhrase(effects)}, so answering again starts a new chat. This chat stays as it is.`;
}

/**
 * The latest message, open for editing in place of its bubble.
 *
 * Enter sends and Shift+Enter starts a new line, as in the composer. Escape
 * puts the message back unchanged. The images and files the message carried
 * go with the edit.
 */
export function UserMessageEditor({
  text,
  attachments,
  sideEffects,
  onCancel,
  onSubmit,
}: {
  text: string;
  /** The message's images and files, shown as they will be sent again. */
  attachments?: ReactNode;
  sideEffects: readonly TurnSideEffect[];
  onCancel: () => void;
  onSubmit: (text: string) => void;
}) {
  const [draft, setDraft] = useState(text);
  const changed = draft.trim().length > 0 && draft.trim() !== text.trim();
  const note = editStartsNewChatCopy(sideEffects);
  const rows = Math.min(12, Math.max(2, draft.split("\n").length));

  function submit() {
    if (changed) onSubmit(draft.trim());
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      onCancel();
      return;
    }
    if (
      event.key === "Enter" &&
      !event.shiftKey &&
      !event.nativeEvent.isComposing
    ) {
      event.preventDefault();
      submit();
    }
  }

  return (
    <form
      className="message-user-editor"
      aria-label="Edit message"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      {attachments}
      <Textarea
        // Focus lands in the field the reader just asked to edit.
        autoFocus
        aria-label="Message"
        className="min-h-11 resize-none rounded-none border-0 bg-transparent p-0 text-md shadow-none focus-visible:border-0 focus-visible:ring-0 md:text-md"
        rows={rows}
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        onKeyDown={onKeyDown}
        onFocus={(event) => {
          const end = event.currentTarget.value.length;
          event.currentTarget.setSelectionRange(end, end);
        }}
      />
      {note && <p className="message-user-editor-note">{note}</p>}
      <div className="message-user-editor-actions">
        <Button type="button" variant="ghost" size="sm" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" size="sm" disabled={!changed}>
          {note ? "Send in new chat" : "Send"}
        </Button>
      </div>
    </form>
  );
}

/**
 * Where a branch's copied history ends, and the way back to the chat it came
 * from.
 */
export function BranchNotice({
  title,
  onOpen,
}: {
  /** The original's title, or null when it has none. */
  title: string | null;
  /** Open the original. Absent when it no longer exists. */
  onOpen?: () => void;
}) {
  return (
    <div className="message-branch-notice" role="note">
      <GitBranch aria-hidden="true" className="message-branch-notice-icon" />
      {onOpen ? (
        <span>
          Branched from{" "}
          <button
            type="button"
            className="message-branch-notice-link"
            onClick={onOpen}
          >
            {title ?? "the original chat"}
          </button>
        </span>
      ) : (
        <span>Branched from a chat that no longer exists</span>
      )}
    </div>
  );
}
