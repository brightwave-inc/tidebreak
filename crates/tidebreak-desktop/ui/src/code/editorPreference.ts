import { useSyncExternalStore } from "react";

/**
 * Which editor "Open in editor" starts, and where its program lives.
 *
 * The choice is this desktop's, not the workspace's or the server's: the same
 * workspace opened from two computers should open in whatever editor each
 * reader installed. So it lives in local storage next to the theme, in the same
 * module-store shape — the settings panel and every menu that reads it mount
 * and unmount independently, and each one holding its own boot-time snapshot is
 * exactly the bug that shape exists to prevent.
 */

export type ExternalEditorId =
  | "vscode"
  | "cursor"
  | "zed"
  | "jetbrains"
  | "custom";

export type EditorPreference = {
  editor: ExternalEditorId;
  /** Absolute program path, used only when `editor` is `custom`. */
  customProgram: string;
};

/** The editors the settings panel offers, in the order it lists them. */
export const EXTERNAL_EDITORS: readonly {
  id: ExternalEditorId;
  label: string;
}[] = [
  { id: "vscode", label: "Visual Studio Code" },
  { id: "cursor", label: "Cursor" },
  { id: "zed", label: "Zed" },
  { id: "jetbrains", label: "JetBrains IDE" },
  { id: "custom", label: "Custom command…" },
];

const STORAGE_KEY = "tidebreak.externalEditor";

const DEFAULT_PREFERENCE: EditorPreference = {
  editor: "vscode",
  customProgram: "",
};

export function isExternalEditorId(value: unknown): value is ExternalEditorId {
  return EXTERNAL_EDITORS.some((editor) => editor.id === value);
}

export function externalEditorLabel(id: ExternalEditorId): string {
  return EXTERNAL_EDITORS.find((editor) => editor.id === id)?.label ?? id;
}

/**
 * What every surface calls the action.
 *
 * "Open in Zed", not "Open in editor": the reader picked one, and naming it is
 * the difference between a menu item they trust and one they have to try. A
 * custom command has no name worth showing, so that one stays generic.
 */
export function openInEditorLabel(
  editor: ExternalEditorId = currentEditorPreference().editor,
): string {
  return editor === "custom"
    ? "Open in editor"
    : `Open in ${externalEditorLabel(editor)}`;
}

/**
 * The stored preference, or the default when nothing is stored.
 *
 * Anything unreadable — a value from an older shape, a hand-edited key — falls
 * back rather than throwing: a corrupt preference should cost the reader one
 * trip to settings, not the menu that reads it.
 */
export function readStoredEditorPreference(): EditorPreference {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(STORAGE_KEY);
  } catch {
    // Ignore storage access failures (private mode, disabled storage).
  }
  if (raw === null) return DEFAULT_PREFERENCE;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object") {
      return DEFAULT_PREFERENCE;
    }
    const value = parsed as Partial<EditorPreference>;
    if (!isExternalEditorId(value.editor)) return DEFAULT_PREFERENCE;
    return {
      editor: value.editor,
      customProgram:
        typeof value.customProgram === "string" ? value.customProgram : "",
    };
  } catch {
    return DEFAULT_PREFERENCE;
  }
}

let state: EditorPreference = readStoredEditorPreference();
const listeners = new Set<() => void>();

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function getSnapshot(): EditorPreference {
  return state;
}

/**
 * The preference in force right now.
 *
 * Not `readStoredEditorPreference`: storage is where the value survives a
 * restart, but the store is where a change made this session already is. The
 * callers outside React — the command list, the native open — have to see the
 * same value the settings panel just set.
 */
export function currentEditorPreference(): EditorPreference {
  return state;
}

export function setEditorPreference(next: EditorPreference): void {
  const value: EditorPreference = {
    editor: next.editor,
    customProgram: next.customProgram.trim(),
  };
  if (
    value.editor === state.editor &&
    value.customProgram === state.customProgram
  ) {
    return;
  }
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
  } catch {
    // Ignore storage access failures; the session still gets the new choice.
  }
  state = value;
  for (const listener of [...listeners]) listener();
}

/** Test seam: drop back to the boot state without touching storage. */
export function resetEditorPreferenceStore(): void {
  state = readStoredEditorPreference();
  for (const listener of [...listeners]) listener();
}

export function useEditorPreference(): EditorPreference {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
