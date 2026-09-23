import { create } from "zustand";

import type { ApiClient } from "../api/client";
import { HttpError } from "../api/client/http";
import type { CodeWorkspaceBlob } from "../api/types";
import { friendlyErrorMessage } from "@/lib/utils";

/**
 * Unsaved edits to workspace files, keyed by workspace and path.
 *
 * The file viewer only mounts while its tab is in front, so the text you are
 * editing lives here, where switching tabs cannot drop it. Other code reads it
 * too: the center tabs mark a file with unsaved changes, and closing a tab,
 * leaving the workspace, or reloading asks before throwing them away. A quit
 * confirmation can ask through {@link unsavedCodeFiles} the same way.
 */

/** Where the last save of a draft stands. */
export type CodeFileSaveState =
  | { kind: "idle" }
  | { kind: "saving" }
  | { kind: "saved" }
  | { kind: "failed"; message: string };

/**
 * The file on disk moved away from the version the editor loaded while the
 * buffer held unsaved changes.
 */
export type CodeFileConflict = {
  /** The hash on disk now, when the server could say. */
  diskHash: string | null;
  /** A save was refused because of it, rather than a reload noticing it. */
  rejected: boolean;
};

export type CodeFileDraft = {
  workspaceId: string;
  path: string;
  /** The text the editor loaded or last saved, exactly as the server has it. */
  baseText: string;
  /** The hash of `baseText`: what a save names as the version it replaces. */
  baseHash: string;
  /**
   * What the buffer holds now, as the editor reports it. Monaco keeps a
   * leading byte-order mark out of the text it reports, so this never has
   * one; {@link draftContent} puts it back for the save.
   */
  text: string;
  save: CodeFileSaveState;
  conflict: CodeFileConflict | null;
  /**
   * The disk version you chose to keep your changes over. A reload that
   * finds the same version on disk does not raise the notice again.
   */
  keptOverHash: string | null;
};

const BYTE_ORDER_MARK = "\uFEFF";

function withoutByteOrderMark(text: string): string {
  return text.startsWith(BYTE_ORDER_MARK) ? text.slice(1) : text;
}

/** The key a draft is filed under. */
export function codeFileDraftKey(workspaceId: string, path: string): string {
  return `${workspaceId}\u0000${path}`;
}

/** The buffer differs from the version it was loaded or last saved as. */
export function isCodeFileDraftDirty(draft: CodeFileDraft): boolean {
  return draft.text !== withoutByteOrderMark(draft.baseText);
}

/** The bytes a save sends: the buffer, with the file's byte-order mark kept. */
export function draftContent(draft: CodeFileDraft): string {
  return draft.baseText.startsWith(BYTE_ORDER_MARK)
    ? BYTE_ORDER_MARK + draft.text
    : draft.text;
}

/** Whether a blob is a file the editor can open and save back. */
export function editableBlob(
  blob: CodeWorkspaceBlob,
): blob is CodeWorkspaceBlob & { hash: string } {
  return (
    blob.hash !== undefined &&
    !blob.binary &&
    !blob.truncated &&
    blob.revision === undefined
  );
}

type DraftsState = {
  drafts: Readonly<Record<string, CodeFileDraft>>;
  /** Open a draft over the version on screen. A draft already open stays. */
  startEditing: (
    workspaceId: string,
    path: string,
    base: { text: string; hash: string },
  ) => void;
  /** Record what the buffer holds after an edit. */
  setText: (workspaceId: string, path: string, text: string) => void;
  /** Drop drafts, unsaved changes and all. */
  discard: (workspaceId: string, paths?: readonly string[]) => void;
  /** Drop drafts with nothing unsaved whose tabs are no longer open. */
  pruneClean: (workspaceId: string, openPaths: ReadonlySet<string>) => void;
  /**
   * A reload of the file landed. A clean draft follows the disk; a draft with
   * unsaved changes keeps them and raises the changed-on-disk notice.
   */
  diskVersion: (
    workspaceId: string,
    path: string,
    blob: CodeWorkspaceBlob,
  ) => void;
  /** Throw the buffer away for the version on disk now. */
  reload: (
    workspaceId: string,
    path: string,
    blob: CodeWorkspaceBlob & { hash: string },
  ) => void;
  /** Keep the buffer and put the notice away for this disk version. */
  keepMine: (workspaceId: string, path: string) => void;
  /** The disk version a comparison just read. */
  noteDiskHash: (
    workspaceId: string,
    path: string,
    hash: string | null,
  ) => void;
  beginSave: (workspaceId: string, path: string) => void;
  saveSucceeded: (
    workspaceId: string,
    path: string,
    saved: { content: string; hash: string },
  ) => void;
  saveConflicted: (
    workspaceId: string,
    path: string,
    diskHash: string | null,
  ) => void;
  saveFailed: (workspaceId: string, path: string, message: string) => void;
};

function updateDraft(
  state: DraftsState,
  workspaceId: string,
  path: string,
  change: (draft: CodeFileDraft) => CodeFileDraft | null,
): Partial<DraftsState> | DraftsState {
  const key = codeFileDraftKey(workspaceId, path);
  const draft = state.drafts[key];
  if (!draft) return state;
  const next = change(draft);
  if (next === draft) return state;
  const drafts = { ...state.drafts };
  if (next) drafts[key] = next;
  else delete drafts[key];
  return { drafts };
}

export const useCodeFileDraftStore = create<DraftsState>()((set) => ({
  drafts: {},
  startEditing: (workspaceId, path, base) =>
    set((state) => {
      const key = codeFileDraftKey(workspaceId, path);
      if (state.drafts[key]) return state;
      return {
        drafts: {
          ...state.drafts,
          [key]: {
            workspaceId,
            path,
            baseText: base.text,
            baseHash: base.hash,
            text: withoutByteOrderMark(base.text),
            save: { kind: "idle" },
            conflict: null,
            keptOverHash: null,
          },
        },
      };
    }),
  setText: (workspaceId, path, text) =>
    set((state) =>
      updateDraft(state, workspaceId, path, (draft) =>
        draft.text === text
          ? draft
          : {
              ...draft,
              text,
              // "Saved" describes the text that was saved, not this one.
              save: draft.save.kind === "saved" ? { kind: "idle" } : draft.save,
            },
      ),
    ),
  discard: (workspaceId, paths) =>
    set((state) => {
      const drafts = { ...state.drafts };
      let changed = false;
      for (const [key, draft] of Object.entries(state.drafts)) {
        if (draft.workspaceId !== workspaceId) continue;
        if (paths && !paths.includes(draft.path)) continue;
        delete drafts[key];
        changed = true;
      }
      return changed ? { drafts } : state;
    }),
  pruneClean: (workspaceId, openPaths) =>
    set((state) => {
      const drafts = { ...state.drafts };
      let changed = false;
      for (const [key, draft] of Object.entries(state.drafts)) {
        if (draft.workspaceId !== workspaceId) continue;
        if (openPaths.has(draft.path) || isCodeFileDraftDirty(draft)) continue;
        if (draft.save.kind === "saving") continue;
        delete drafts[key];
        changed = true;
      }
      return changed ? { drafts } : state;
    }),
  diskVersion: (workspaceId, path, blob) =>
    set((state) =>
      updateDraft(state, workspaceId, path, (draft) => {
        // A save in flight settles the base itself; a read that raced it
        // says nothing the save's answer will not.
        if (draft.save.kind === "saving") return draft;
        const diskHash = blob.hash ?? null;
        if (diskHash !== null && diskHash === draft.baseHash) {
          return draft.conflict && !draft.conflict.rejected
            ? { ...draft, conflict: null }
            : draft;
        }
        if (!isCodeFileDraftDirty(draft)) {
          // Nothing to lose: follow the disk, or stop editing a file that is
          // no longer one the editor can save.
          if (!editableBlob(blob)) return null;
          return {
            ...draft,
            baseText: blob.content,
            baseHash: blob.hash,
            text: withoutByteOrderMark(blob.content),
            conflict: null,
            keptOverHash: null,
          };
        }
        if (diskHash !== null && diskHash === draft.keptOverHash) return draft;
        if (draft.conflict?.diskHash === diskHash) return draft;
        return {
          ...draft,
          conflict: {
            diskHash,
            rejected: draft.conflict?.rejected ?? false,
          },
        };
      }),
    ),
  reload: (workspaceId, path, blob) =>
    set((state) =>
      updateDraft(state, workspaceId, path, (draft) => ({
        ...draft,
        baseText: blob.content,
        baseHash: blob.hash,
        text: withoutByteOrderMark(blob.content),
        save: { kind: "idle" },
        conflict: null,
        keptOverHash: null,
      })),
    ),
  keepMine: (workspaceId, path) =>
    set((state) =>
      updateDraft(state, workspaceId, path, (draft) =>
        draft.conflict
          ? {
              ...draft,
              conflict: null,
              keptOverHash: draft.conflict.diskHash,
            }
          : draft,
      ),
    ),
  noteDiskHash: (workspaceId, path, hash) =>
    set((state) =>
      updateDraft(state, workspaceId, path, (draft) =>
        draft.conflict && draft.conflict.diskHash !== hash
          ? { ...draft, conflict: { ...draft.conflict, diskHash: hash } }
          : draft,
      ),
    ),
  beginSave: (workspaceId, path) =>
    set((state) =>
      updateDraft(state, workspaceId, path, (draft) => ({
        ...draft,
        save: { kind: "saving" },
      })),
    ),
  saveSucceeded: (workspaceId, path, saved) =>
    set((state) =>
      updateDraft(state, workspaceId, path, (draft) => ({
        ...draft,
        baseText: saved.content,
        baseHash: saved.hash,
        save: { kind: "saved" },
        conflict: null,
        keptOverHash: null,
      })),
    ),
  saveConflicted: (workspaceId, path, diskHash) =>
    set((state) =>
      updateDraft(state, workspaceId, path, (draft) => ({
        ...draft,
        save: { kind: "idle" },
        conflict: { diskHash, rejected: true },
      })),
    ),
  saveFailed: (workspaceId, path, message) =>
    set((state) =>
      updateDraft(state, workspaceId, path, (draft) => ({
        ...draft,
        save: { kind: "failed", message },
      })),
    ),
}));

/** One draft, or undefined when the file is not being edited. */
export function useCodeFileDraft(
  workspaceId: string,
  path: string,
): CodeFileDraft | undefined {
  return useCodeFileDraftStore(
    (state) => state.drafts[codeFileDraftKey(workspaceId, path)],
  );
}

/** Paths in one workspace whose buffers hold unsaved changes, sorted. */
export function dirtyCodeFilePaths(
  state: Pick<DraftsState, "drafts">,
  workspaceId: string,
): string[] {
  return Object.values(state.drafts)
    .filter(
      (draft) =>
        draft.workspaceId === workspaceId && isCodeFileDraftDirty(draft),
    )
    .map((draft) => draft.path)
    .sort();
}

/** Every file with unsaved changes, in every workspace. */
export function unsavedCodeFiles(
  state: Pick<DraftsState, "drafts">,
): { workspaceId: string; path: string }[] {
  return Object.values(state.drafts)
    .filter(isCodeFileDraftDirty)
    .map(({ workspaceId, path }) => ({ workspaceId, path }))
    .sort((left, right) =>
      left.workspaceId === right.workspaceId
        ? left.path.localeCompare(right.path)
        : left.workspaceId.localeCompare(right.workspaceId),
    );
}

/**
 * The paths in one workspace with unsaved changes, as a set that only changes
 * identity when its members do, so a tab strip can hold it without
 * re-rendering on every keystroke.
 */
export function useDirtyCodeFilePaths(
  workspaceId: string,
): ReadonlySet<string> {
  const joined = useCodeFileDraftStore((state) =>
    dirtyCodeFilePaths(state, workspaceId).join("\u0000"),
  );
  return dirtyPathSet(joined);
}

const dirtyPathSets = new Map<string, ReadonlySet<string>>();

function dirtyPathSet(joined: string): ReadonlySet<string> {
  const cached = dirtyPathSets.get(joined);
  if (cached) return cached;
  const set: ReadonlySet<string> = new Set(
    joined ? joined.split("\u0000") : [],
  );
  // A handful of live sets at most; keep the cache from growing without end.
  if (dirtyPathSets.size > 64) dirtyPathSets.clear();
  dirtyPathSets.set(joined, set);
  return set;
}

/** The file's last path segment, for prompts that name it. */
export function codeFileName(path: string): string {
  return path.split("/").pop() || path;
}

/**
 * The question to ask before unsaved changes are thrown away, naming the file
 * when there is one.
 */
export function discardUnsavedTitle(paths: readonly string[]): string {
  const [only] = paths;
  return paths.length === 1 && only
    ? `Discard unsaved changes to ${codeFileName(only)}?`
    : `Discard unsaved changes to ${paths.length} files?`;
}

/** A save refused because the file on disk moved: `409 file_changed`. */
export class CodeFileChangedError extends HttpError {
  constructor(
    source: HttpError,
    /** The hash on disk now, when the server named a well-formed one. */
    readonly diskHash: string | null,
  ) {
    super(source.status, source.message, source.kind, source.body);
    this.name = "CodeFileChangedError";
  }
}

const CONTENT_HASH = /^[0-9a-f]{64}$/;

/** Turn a `409 file_changed` into the error the editor branches on. */
export function codeFileChangedError(
  error: unknown,
): CodeFileChangedError | null {
  if (!(error instanceof HttpError)) return null;
  if (error.status !== 409 || error.kind !== "file_changed") return null;
  if (error instanceof CodeFileChangedError) return error;
  const hash = error.body?.current_hash;
  return new CodeFileChangedError(
    error,
    typeof hash === "string" && CONTENT_HASH.test(hash) ? hash : null,
  );
}

export type CodeFileSaveOutcome =
  | { kind: "saved"; content: string; hash: string }
  | { kind: "conflict" }
  | { kind: "failed" }
  | { kind: "skipped" };

/**
 * Save one draft. Every answer lands in the store rather than in a
 * component, so a save that finishes after you switched tabs still records
 * what happened to your text.
 *
 * `baseHash` overrides the draft's own base: Overwrite saves against the
 * version on disk now, which you confirmed replacing.
 */
export async function saveCodeFileDraft(
  client: Pick<ApiClient, "saveCodeWorkspaceFile">,
  workspaceId: string,
  path: string,
  options: { baseHash?: string } = {},
): Promise<CodeFileSaveOutcome> {
  const store = useCodeFileDraftStore.getState();
  const draft = store.drafts[codeFileDraftKey(workspaceId, path)];
  if (!draft || draft.save.kind === "saving") return { kind: "skipped" };
  const content = draftContent(draft);
  store.beginSave(workspaceId, path);
  try {
    const saved = await client.saveCodeWorkspaceFile(workspaceId, {
      path,
      content,
      base_hash: options.baseHash ?? draft.baseHash,
    });
    useCodeFileDraftStore
      .getState()
      .saveSucceeded(workspaceId, path, { content, hash: saved.hash });
    return { kind: "saved", content, hash: saved.hash };
  } catch (error) {
    const changed = codeFileChangedError(error);
    if (changed) {
      useCodeFileDraftStore
        .getState()
        .saveConflicted(workspaceId, path, changed.diskHash);
      return { kind: "conflict" };
    }
    useCodeFileDraftStore
      .getState()
      .saveFailed(
        workspaceId,
        path,
        friendlyErrorMessage(error, "Could not save the file."),
      );
    return { kind: "failed" };
  }
}
