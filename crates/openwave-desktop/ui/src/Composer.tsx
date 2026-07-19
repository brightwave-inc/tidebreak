import {
  type ChangeEvent,
  type KeyboardEvent,
  useLayoutEffect,
  useRef,
} from "react";
import { MAX_STEER_CHARACTERS } from "./ActiveTurnSteer";

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

export type ComposerProps = {
  activeTurnId: string | null;
  busy: boolean;
  cancelError: string | null;
  cancelPending: boolean;
  disabled: boolean;
  draft: string;
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
  const inputDisabled = disabled;
  const active = busy && activeTurnId !== null;
  const hasDraft = Boolean(draft.trim());
  const steerHasUnsupportedCharacter = active && draft.includes("\0");
  const steerTooLong =
    active && [...draft.trim()].length > MAX_STEER_CHARACTERS;
  const canSubmit =
    !inputDisabled &&
    !steerPending &&
    !cancelPending &&
    hasDraft &&
    !steerHasUnsupportedCharacter &&
    !steerTooLong &&
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

  return (
    <form
      className="composer"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <textarea
        ref={textareaRef}
        value={draft}
        placeholder={
          active ? "Guide the active response…" : "Message OpenWave…"
        }
        aria-label="Message"
        disabled={inputDisabled}
        onChange={onChange}
        onKeyDown={(event) => {
          if (!shouldSubmitComposerKey(event.nativeEvent)) return;
          event.preventDefault();
          void submit();
        }}
      />
      <div className="composer-actions">
        {active ? (
          <>
            {(hasDraft || steerPending) && (
              <button
                type="submit"
                className="btn btn-primary"
                aria-label="Redirect active response"
                disabled={!canSubmit}
              >
                {steerPending ? "Sending…" : "Redirect"}
              </button>
            )}
            <button
              type="button"
              className="btn btn-stop"
              aria-label={
                cancelPending ? "Stopping response" : "Stop response"
              }
              disabled={disabled || cancelPending}
              onClick={() => void onStop()}
            >
              {cancelPending ? "Stopping…" : "Stop"}
            </button>
          </>
        ) : (
          <button
            type="submit"
            className="btn btn-primary"
            disabled={!canSubmit}
          >
            Send
          </button>
        )}
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
    </form>
  );
}
