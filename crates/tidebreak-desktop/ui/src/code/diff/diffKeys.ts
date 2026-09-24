/**
 * The keys a diff answers while it has focus, and the list the shortcuts
 * dialog shows for them.
 *
 * Next and previous file follow the review sites people already use: J and K
 * on GitHub, with ] and [ as GitLab spells them. W hides whitespace changes,
 * as it does on GitHub. They are single keys rather than chords because they
 * only act while focus is in the diff, never in a text field or a dialog, so
 * they cannot swallow typing.
 */

export type DiffKeyId =
  | "next-file"
  | "previous-file"
  | "toggle-whitespace"
  | "move-line"
  | "extend-lines"
  | "comment"
  | "save-comment";

export type DiffKey = {
  id: DiffKeyId;
  /** What the key does, phrased for the shortcuts dialog. */
  description: string;
  /** The keycaps in the order they are pressed, for a Cmd or a Ctrl keyboard. */
  keycaps: (command: boolean) => readonly string[];
};

export const DIFF_KEYS: readonly DiffKey[] = [
  { id: "next-file", description: "Next file", keycaps: () => ["J"] },
  { id: "previous-file", description: "Previous file", keycaps: () => ["K"] },
  {
    id: "toggle-whitespace",
    description: "Hide or show whitespace changes",
    keycaps: () => ["W"],
  },
  {
    id: "move-line",
    description: "Move between line numbers",
    keycaps: () => ["↓"],
  },
  {
    id: "extend-lines",
    description: "Select more lines to comment on",
    keycaps: (command) => [command ? "⇧" : "Shift", "↓"],
  },
  {
    id: "comment",
    description: "Comment on the selected lines",
    keycaps: () => ["↩"],
  },
  {
    id: "save-comment",
    description: "Add the comment",
    keycaps: (command) => [command ? "⌘" : "Ctrl", "↩"],
  },
];

type DiffKeyEvent = Pick<
  KeyboardEvent,
  "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey" | "target"
>;

/**
 * Whether a key belongs to something other than the diff: a field being
 * typed in, editable content, or a dialog or menu open over the diff. React
 * passes a portal's keys up to the diff that opened it, so a key pressed in
 * the "Delete this comment?" dialog would otherwise flip whitespace.
 */
function isElsewhere(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName)) return true;
  if (target.isContentEditable) return true;
  return (
    target.closest(
      '[role="dialog"], [role="alertdialog"], [role="menu"], [role="listbox"], [contenteditable]:not([contenteditable="false"])',
    ) !== null
  );
}

/** The file-level action a key asks for, or null when it asks for none. */
export function diffFileKey(
  event: DiffKeyEvent,
): "next-file" | "previous-file" | "toggle-whitespace" | null {
  if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) {
    return null;
  }
  if (isElsewhere(event.target)) return null;
  switch (event.key) {
    case "j":
    case "J":
    case "]":
      return "next-file";
    case "k":
    case "K":
    case "[":
      return "previous-file";
    case "w":
    case "W":
      return "toggle-whitespace";
    default:
      return null;
  }
}

/**
 * Move to the next or previous file of a multi-file diff: scroll its section
 * to the top and focus its header. Returns false when there is no file that
 * way, so the caller can step to another diff instead.
 *
 * Files are the elements marked `data-diff-file`, each with one control
 * marked `data-diff-file-header`. The current file is the one holding focus,
 * or else the first one whose top is at or below the top of `scroller`.
 */
export function stepDiffFile(
  container: HTMLElement,
  direction: 1 | -1,
  scroller: HTMLElement = container,
): boolean {
  const files = [
    ...container.querySelectorAll<HTMLElement>("[data-diff-file]"),
  ];
  if (files.length === 0) return false;
  const focused = document.activeElement;
  let current = files.findIndex(
    (file) => focused instanceof Node && file.contains(focused),
  );
  if (current === -1) {
    // Nothing focused yet: the reader is in the file at the top of the view.
    const top = scroller.getBoundingClientRect().top;
    const below = files.findIndex(
      (file) => file.getBoundingClientRect().top >= top - 1,
    );
    if (below === -1) current = files.length - 1;
    else if (files[below]!.getBoundingClientRect().top > top + 1) {
      current = below - 1;
    } else current = below;
  }
  const target = files[current + direction];
  if (!target) return false;
  target.scrollIntoView?.({ block: "start" });
  target
    .querySelector<HTMLElement>("[data-diff-file-header]")
    ?.focus({ preventScroll: true });
  return true;
}
