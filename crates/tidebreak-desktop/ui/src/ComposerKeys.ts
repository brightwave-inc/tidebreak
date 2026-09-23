/**
 * The keys the message composer answers while you type, and the one rule of
 * theirs that is not already the composer's own: Escape stops a running turn.
 *
 * The list is what the shortcuts dialog shows under Composer. The composer
 * handles the keys itself rather than through the shell shortcut table,
 * because they mean something only while the field has focus, so the dialog
 * reads them from here.
 */

export type ComposerKeyId =
  | "send"
  | "new-line"
  | "steer"
  | "stop"
  | "recall"
  | "command"
  | "mention";

export type ComposerKey = {
  id: ComposerKeyId;
  /** What the key does, phrased for the shortcuts dialog. */
  description: string;
  /** The keycaps in the order they are pressed, for a Cmd or a Ctrl keyboard. */
  keycaps: (command: boolean) => readonly string[];
};

export const COMPOSER_KEYS: readonly ComposerKey[] = [
  { id: "send", description: "Send the message", keycaps: () => ["↩"] },
  {
    id: "new-line",
    description: "Start a new line",
    keycaps: (command) => [command ? "⇧" : "Shift", "↩"],
  },
  {
    id: "steer",
    description: "Steer the running turn",
    keycaps: (command) => [command ? "⌘" : "Ctrl", "↩"],
  },
  { id: "stop", description: "Stop the running turn", keycaps: () => ["Esc"] },
  {
    id: "recall",
    description: "Recall a message you sent",
    keycaps: () => ["↑"],
  },
  {
    id: "command",
    description: "Use a command or skill",
    keycaps: () => ["/"],
  },
  {
    id: "mention",
    description: "Mention a file or folder",
    keycaps: () => ["@"],
  },
];

type StopKeyEvent = Pick<
  KeyboardEvent,
  | "altKey"
  | "ctrlKey"
  | "defaultPrevented"
  | "isComposing"
  | "key"
  | "keyCode"
  | "metaKey"
  | "shiftKey"
>;

/** A dialog, menu, or list open anywhere on screen, drawn by Radix. */
const OPEN_LAYER = ["dialog", "alertdialog", "menu", "listbox"]
  .map((role) => `[role="${role}"][data-state="open"]`)
  .join(", ");

/**
 * Whether a keydown in the composer asks to stop the running turn.
 *
 * Escape belongs first to whatever is open over the composer, so a menu, a
 * list, or a dialog closes on it and nothing stops. An open Radix layer takes
 * Escape on the document before the field sees it and marks the event handled,
 * which is what `defaultPrevented` reads; the document check covers a layer
 * that is still open without having taken the key. An IME composition cancels
 * on Escape as well, so a key that belongs to one is left to it. The caller
 * checks its own `/` and `@` lists first, because those close on Escape too.
 */
export function shouldStopTurnKey(event: StopKeyEvent, doc: Document): boolean {
  return (
    event.key === "Escape" &&
    !event.defaultPrevented &&
    !event.isComposing &&
    event.keyCode !== 229 &&
    !event.altKey &&
    !event.ctrlKey &&
    !event.metaKey &&
    !event.shiftKey &&
    doc.querySelector(OPEN_LAYER) === null
  );
}
