import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import Editor, { DiffEditor, type OnMount } from "@monaco-editor/react";
import { Check, CircleAlert, Eye, Lock, Pencil } from "lucide-react";
import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import type { CodeWorkspaceBlob } from "../api/types";
import { useConfirm } from "@/components/ConfirmDialog";
import { ImageViewer } from "@/components/document/image-viewer";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import type { FileBytesSource } from "@/document/useFileDownload";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { MessageMarkdown } from "@/MessageMarkdown";
import { useTheme } from "@/theme";
import {
  codeFileName,
  discardUnsavedTitle,
  editableBlob,
  isCodeFileDraftDirty,
  saveCodeFileDraft,
  useCodeFileDraft,
  useCodeFileDraftStore,
  type CodeFileDraft,
  type CodeFileSaveOutcome,
} from "./CodeFileDraftStore";
import { noteWorkspaceFilesChanged } from "./CodeUpdatesStore";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { configureMonaco, monacoLanguage, monacoTheme } from "./monacoEnv";
import { MiddleTruncate } from "./MiddleTruncate";
import { STATUS_MARK } from "./statusTone";
import { HEADER_CAPTION, WorkspaceRevisionChip } from "./WorkspaceRevisionChip";
import { OpenInEditorButton } from "./OpenInEditorButton";
import { useLiveResource } from "./useLiveContent";

type FileViewerClient = Pick<
  ApiClient,
  "getCodeWorkspaceBlob" | "getCodeWorkspaceFile" | "saveCodeWorkspaceFile"
>;

/** What the body shows: the text as written, or the markdown it renders. */
type FileView = "source" | "preview";

/** Markdown files get a Preview toggle. Every other file shows its source. */
export function isMarkdownPath(path: string): boolean {
  return /\.(md|mdx|markdown)$/i.test(path);
}

/**
 * Why a file opens read-only, as the header says it, or null when you can
 * edit it.
 *
 * `pageReason` is what the page knows before the file loads, such as a
 * sandbox workspace. The rest follows from the file: a sandbox checkout, an
 * image or other binary, a file the viewer cut short, and text that is not
 * UTF-8 all stay read-only, because saving any of them would write something
 * other than what is on screen.
 */
export function readOnlyReason(
  blob: CodeWorkspaceBlob | null,
  pageReason?: string,
): string | null {
  if (pageReason) return pageReason;
  if (!blob) return null;
  if (blob.revision === "retained") return "Saved checkpoints are read-only";
  if (blob.revision !== undefined) return "Sandbox files are read-only";
  if (blob.binary) {
    return imageMediaTypeForPath(blob.path)
      ? "Images are read-only"
      : "Binary files are read-only";
  }
  if (blob.truncated) return "Files over 512 KB are read-only";
  if (blob.hash === undefined) return "Files that are not UTF-8 are read-only";
  return null;
}

/**
 * One worktree file in a Monaco view, and the editor for it.
 *
 * Text files open read-only with an Edit button. Editing keeps the buffer in
 * `CodeFileDraftStore`, so it outlives this component when you switch tabs,
 * and saves it with the hash the file was loaded at. When an agent changes
 * the same file, the buffer stays yours: a reload that finds the file changed
 * raises a notice, and a save the server refuses offers to compare and to
 * overwrite.
 */
export function FileViewer({
  client,
  workspaceId,
  path,
  contentRevision = 0,
  revealLine,
  revealRevision = 0,
  onOpenInEditor,
  readOnlyReason: pageReadOnlyReason,
}: {
  client: FileViewerClient;
  workspaceId: string;
  path: string;
  contentRevision?: number;
  revealLine?: number;
  revealRevision?: number;
  /** Hand this file, at the line on screen, to the reader's own editor. */
  onOpenInEditor?: (path: string, line?: number) => void;
  /**
   * Why the page will not let you edit this file, such as a sandbox
   * workspace. Shown in the header in place of the Edit button.
   */
  readOnlyReason?: string;
}) {
  const { resolved: resolvedTheme } = useTheme();
  const load = useCallback(
    () => client.getCodeWorkspaceBlob(workspaceId, path),
    [client, workspaceId, path],
  );
  const resource = useLiveResource({
    key: `${workspaceId}:${path}`,
    revision: contentRevision,
    load,
    errorMessage: "Could not open that file",
  });
  const { data, error, refreshing, adopt } = resource;
  const draft = useCodeFileDraft(workspaceId, path);
  const editing = draft !== undefined;
  const dirty = draft !== undefined && isCodeFileDraftDirty(draft);
  const saving = draft?.save.kind === "saving";
  const markdown = isMarkdownPath(path);
  const [view, setView] = useState<FileView>(() =>
    markdown && revealLine === undefined && !editing ? "preview" : "source",
  );
  const [comparison, setComparison] = useState<string | null>(null);
  const [comparing, setComparing] = useState(false);
  const { confirm, dialog } = useConfirm();
  const mounted = useRef(true);

  useEffect(() => {
    configureMonaco();
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  // Every read of the file is also a check on the draft: a clean buffer
  // follows the disk, an unsaved one keeps its text and raises the notice.
  useEffect(() => {
    if (data) {
      useCodeFileDraftStore.getState().diskVersion(workspaceId, path, data);
    }
  }, [data, workspaceId, path]);

  // Revealing a line needs the source, not the rendered page.
  useEffect(() => {
    if (revealLine !== undefined) setView("source");
  }, [revealLine, revealRevision]);

  const blocker = readOnlyReason(data, pageReadOnlyReason);
  const canEdit = data !== null && blocker === null && editableBlob(data);

  function startEditing() {
    if (!data || !editableBlob(data)) return;
    useCodeFileDraftStore
      .getState()
      .startEditing(workspaceId, path, { text: data.content, hash: data.hash });
    setView("source");
  }

  function settle(outcome: CodeFileSaveOutcome) {
    if (outcome.kind !== "saved") return;
    noteWorkspaceFilesChanged(workspaceId);
    if (!mounted.current) return;
    setComparison(null);
    // The saved text is the file now. Adopting it also drops any read that
    // started before the save, so it cannot land and look like a change.
    if (data) {
      adopt({
        ...data,
        content: outcome.content,
        hash: outcome.hash,
        truncated: false,
        binary: false,
      });
    }
  }

  async function save() {
    if (!draft || saving) return;
    settle(await saveCodeFileDraft(client, workspaceId, path));
  }

  async function overwrite() {
    const diskHash = draft?.conflict?.diskHash;
    if (!diskHash || saving) return;
    const confirmed = await confirm({
      title: `Overwrite ${codeFileName(path)}?`,
      description:
        "Your changes replace the version on disk. The changes made there since you opened the file are lost.",
      confirmLabel: "Overwrite",
      destructive: true,
    });
    if (!confirmed) return;
    settle(
      await saveCodeFileDraft(client, workspaceId, path, {
        baseHash: diskHash,
      }),
    );
  }

  async function discard() {
    if (!draft || saving) return;
    if (isCodeFileDraftDirty(draft)) {
      const confirmed = await confirm({
        title: discardUnsavedTitle([path]),
        description: "Your edits since the last save are lost.",
        confirmLabel: "Discard",
        destructive: true,
      });
      if (!confirmed) return;
    }
    useCodeFileDraftStore.getState().discard(workspaceId, [path]);
    setComparison(null);
  }

  /** Read the version on disk now, and hand it to the draft and the view. */
  async function readDisk(): Promise<CodeWorkspaceBlob | null> {
    try {
      const blob = await client.getCodeWorkspaceBlob(workspaceId, path);
      if (mounted.current) adopt(blob);
      else
        useCodeFileDraftStore.getState().diskVersion(workspaceId, path, blob);
      return blob;
    } catch (err) {
      toast.error(
        friendlyErrorMessage(err, "Could not read the file on disk."),
      );
      return null;
    }
  }

  async function reloadFromDisk() {
    const blob = await readDisk();
    if (!blob) return;
    const drafts = useCodeFileDraftStore.getState();
    if (editableBlob(blob)) drafts.reload(workspaceId, path, blob);
    else drafts.discard(workspaceId, [path]);
    setComparison(null);
  }

  function keepMine() {
    useCodeFileDraftStore.getState().keepMine(workspaceId, path);
    setComparison(null);
  }

  async function toggleComparison() {
    if (comparison !== null) {
      setComparison(null);
      return;
    }
    setComparing(true);
    const blob = await readDisk();
    if (mounted.current) setComparing(false);
    if (!blob || !mounted.current) return;
    if (blob.binary || blob.truncated) {
      toast.error("The version on disk cannot be compared here.");
      return;
    }
    setComparison(blob.content);
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const saveChord =
      (event.metaKey || event.ctrlKey) &&
      !event.altKey &&
      !event.shiftKey &&
      (event.code === "KeyS" || event.key.toLowerCase() === "s");
    if (!saveChord || !editing) return;
    event.preventDefault();
    if (dirty) void save();
  }

  const showPreview = markdown && view === "preview";

  return (
    <div
      className="flex min-h-0 flex-1 flex-col overflow-hidden"
      onKeyDown={onKeyDown}
    >
      {dialog}
      {/*
        Deliberately the same two-line header as `DiffPanel`: a heading over a
        mono caption, the same padding, the same fixed spinner slot. The two
        panels share one center tab strip, so any difference in their header
        height shows up as the file body jumping when the reader switches tabs.
      */}
      <header className="flex shrink-0 flex-wrap items-center justify-between gap-x-2 gap-y-1 border-b px-3 py-2">
        <div className={HEADER_CAPTION}>
          <h2 className="text-sm font-medium">File</h2>
          <MiddleTruncate
            text={path}
            className="text-muted-foreground font-mono text-xs"
          />
        </div>
        <div className="flex shrink-0 flex-wrap items-center justify-end gap-x-2 gap-y-1">
          <WorkspaceRevisionChip
            revision={data?.revision}
            revisionRef={data?.revision_ref}
            savedAt={data?.revision_saved_at}
          />
          <FileStatus draft={draft} blocker={editing ? null : blocker} />
          {markdown && (data || editing) && (
            <PreviewToggle
              pressed={showPreview}
              onPressedChange={(pressed) =>
                setView(pressed ? "preview" : "source")
              }
            />
          )}
          {editing ? (
            <EditActions
              dirty={dirty}
              saving={saving}
              onDone={() => void discard()}
              onSave={() => void save()}
            />
          ) : (
            canEdit && (
              <button
                type="button"
                className={cn(
                  "text-muted-foreground hover:bg-muted hover:text-foreground flex cursor-pointer items-center gap-1 rounded-md px-1.5 py-1 text-xs",
                  FOCUS_RING_TIGHT,
                  HOVER_TINT,
                )}
                onClick={startEditing}
              >
                <Pencil className="size-3" aria-hidden />
                Edit
              </button>
            )
          )}
          {onOpenInEditor && (
            <OpenInEditorButton
              onClick={() => onOpenInEditor(path, revealLine)}
            />
          )}
          {/* A fixed trailing slot, so a refresh moves nothing and an idle
              slot never opens a gap between the chip and its neighbors. */}
          <span className="grid size-3.5 shrink-0 place-items-center">
            {refreshing && (
              <Spinner className="size-3.5" aria-label="Refreshing" />
            )}
          </span>
        </div>
      </header>
      {draft?.conflict && (
        <ConflictNotice
          rejected={draft.conflict.rejected}
          canOverwrite={draft.conflict.diskHash !== null}
          comparing={comparison !== null}
          busy={saving || comparing}
          onReload={() => void reloadFromDisk()}
          onKeep={keepMine}
          onCompare={() => void toggleComparison()}
          onOverwrite={() => void overwrite()}
        />
      )}
      {draft?.save.kind === "failed" && (
        <div
          role="alert"
          className="notice-surface notice-critical flex shrink-0 items-start gap-1.5 border-b px-3 py-2 text-sm"
        >
          <CircleAlert
            className={cn("mt-0.5 size-3.5 shrink-0", STATUS_MARK.critical)}
            aria-hidden
          />
          <span>Your changes are not saved. {draft.save.message}</span>
        </div>
      )}
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      {!data && !error && !draft && (
        <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
          <Skeleton className="h-4 w-1/3" />
          <Skeleton className="h-4 w-3/4" />
          <Skeleton className="h-4 w-2/3" />
        </div>
      )}
      {(data || draft) && (
        <BlobBody
          client={client}
          workspaceId={workspaceId}
          contentRevision={contentRevision}
          path={path}
          blob={data}
          draft={draft}
          theme={resolvedTheme}
          revealLine={revealLine}
          revealRevision={revealRevision}
          preview={showPreview}
          comparison={comparison}
        />
      )}
    </div>
  );
}

/** What the header says about the file's state, beside its controls. */
function FileStatus({
  draft,
  blocker,
}: {
  draft: CodeFileDraft | undefined;
  blocker: string | null;
}) {
  const quiet = "text-muted-foreground flex items-center gap-1 text-xs";
  if (draft?.save.kind === "saving") {
    return (
      <span className={quiet} role="status">
        <Spinner className="size-3" aria-hidden />
        Saving…
      </span>
    );
  }
  if (draft && isCodeFileDraftDirty(draft)) {
    return (
      <span className={quiet} role="status">
        <span className="bg-foreground size-1.5 rounded-full" aria-hidden />
        Unsaved changes
      </span>
    );
  }
  if (draft?.save.kind === "saved") {
    return (
      <span className={quiet} role="status">
        <Check className={cn("size-3", STATUS_MARK.ready)} aria-hidden />
        Saved
      </span>
    );
  }
  if (blocker) {
    return (
      <span className={quiet}>
        <Lock className="size-3" aria-hidden />
        {blocker}
      </span>
    );
  }
  return null;
}

/** The markdown switch between the text as written and what it renders. */
function PreviewToggle({
  pressed,
  onPressedChange,
}: {
  pressed: boolean;
  onPressedChange: (pressed: boolean) => void;
}) {
  return (
    <button
      type="button"
      aria-pressed={pressed}
      className={cn(
        "flex cursor-pointer items-center gap-1 rounded-md px-1.5 py-1 text-xs",
        pressed
          ? "bg-muted text-foreground"
          : "text-muted-foreground hover:bg-muted hover:text-foreground",
        FOCUS_RING_TIGHT,
        HOVER_TINT,
      )}
      onClick={() => onPressedChange(!pressed)}
    >
      <Eye className="size-3" aria-hidden />
      Preview
    </button>
  );
}

/** Save and its way out: Discard while there is something to lose, Done after. */
function EditActions({
  dirty,
  saving,
  onDone,
  onSave,
}: {
  dirty: boolean;
  saving: boolean;
  onDone: () => void;
  onSave: () => void;
}) {
  return (
    <div className="flex items-center gap-1">
      <Button
        type="button"
        variant="ghost"
        size="xs"
        disabled={saving}
        onClick={onDone}
      >
        {dirty ? "Discard" : "Done"}
      </Button>
      <Button
        type="button"
        size="xs"
        disabled={!dirty || saving}
        title="Save (⌘S)"
        onClick={onSave}
      >
        {saving && <Spinner className="size-3 text-current" aria-hidden />}
        {saving ? "Saving…" : "Save"}
      </Button>
    </div>
  );
}

/**
 * The file moved on disk under unsaved changes. Reload takes the disk's
 * version; Keep my changes puts the notice away. After a refused save the
 * notice also offers Compare and Overwrite.
 */
function ConflictNotice({
  rejected,
  canOverwrite,
  comparing,
  busy,
  onReload,
  onKeep,
  onCompare,
  onOverwrite,
}: {
  rejected: boolean;
  canOverwrite: boolean;
  comparing: boolean;
  busy: boolean;
  onReload: () => void;
  onKeep: () => void;
  onCompare: () => void;
  onOverwrite: () => void;
}) {
  return (
    <div
      role="alert"
      className="notice-surface notice-warning flex shrink-0 flex-wrap items-center justify-between gap-x-3 gap-y-2 border-b px-3 py-2 text-sm"
    >
      <span className="flex min-w-0 items-start gap-1.5">
        <CircleAlert
          className={cn("mt-0.5 size-3.5 shrink-0", STATUS_MARK.warning)}
          aria-hidden
        />
        <span>
          This file changed on disk.
          {rejected && " Your changes are not saved."}
        </span>
      </span>
      <div className="flex flex-wrap items-center gap-1.5">
        <Button
          type="button"
          variant="outline"
          size="xs"
          disabled={busy}
          onClick={onReload}
        >
          Reload
        </Button>
        <Button
          type="button"
          variant="outline"
          size="xs"
          disabled={busy}
          onClick={onKeep}
        >
          Keep my changes
        </Button>
        {rejected && (
          <>
            <Button
              type="button"
              variant="outline"
              size="xs"
              aria-pressed={comparing}
              disabled={busy && !comparing}
              onClick={onCompare}
            >
              {comparing ? "Hide comparison" : "Compare"}
            </Button>
            <Button
              type="button"
              variant="destructive"
              size="xs"
              disabled={busy || !canOverwrite}
              onClick={onOverwrite}
            >
              Overwrite
            </Button>
          </>
        )}
      </div>
    </div>
  );
}

function BlobBody({
  client,
  workspaceId,
  contentRevision,
  path,
  blob,
  draft,
  theme,
  revealLine,
  revealRevision,
  preview,
  comparison,
}: {
  client: Pick<ApiClient, "getCodeWorkspaceFile">;
  workspaceId: string;
  contentRevision: number;
  path: string;
  blob: CodeWorkspaceBlob | null;
  draft: CodeFileDraft | undefined;
  theme: "light" | "dark";
  revealLine?: number;
  revealRevision: number;
  preview: boolean;
  /** The version on disk to compare the draft against, while comparing. */
  comparison: string | null;
}) {
  const editorRef = useRef<Parameters<OnMount>[0] | null>(null);
  const revealFrameRef = useRef<number | null>(null);
  const editing = draft !== undefined;

  const reveal = useCallback((line: number | undefined) => {
    const editor = editorRef.current;
    if (!editor || line === undefined) return;
    if (revealFrameRef.current !== null) {
      window.cancelAnimationFrame(revealFrameRef.current);
    }
    const revealNow = () => {
      const model = editor.getModel();
      if (!model) return;
      const lineNumber = Math.min(
        model.getLineCount(),
        Math.max(1, Math.round(line)),
      );
      editor.setPosition({ lineNumber, column: 1 });
      editor.revealLineInCenter(lineNumber);
    };

    // @monaco-editor/react can invoke onMount just before it applies the
    // controlled value to a newly-created model. Reveal once immediately and
    // once after the next paint so the first search result opened in a fresh
    // tab is not reset to line 1 by that initial model update.
    revealNow();
    revealFrameRef.current = window.requestAnimationFrame(() => {
      revealFrameRef.current = null;
      revealNow();
    });
  }, []);

  useEffect(() => {
    return () => {
      if (revealFrameRef.current !== null) {
        window.cancelAnimationFrame(revealFrameRef.current);
      }
    };
  }, []);

  useEffect(() => {
    reveal(revealLine);
  }, [reveal, revealLine, revealRevision]);

  const onChange = useCallback(
    (value: string | undefined) => {
      if (value === undefined) return;
      useCodeFileDraftStore.getState().setText(workspaceId, path, value);
    },
    [workspaceId, path],
  );

  const options = useMemo(
    () => ({
      readOnly: !editing,
      minimap: { enabled: false },
      scrollBeyondLastLine: false,
      // The same 13px/20px as the diff body. The two views open the same
      // file from the same tab strip, so a reader flipping between them
      // should see the same lines in the same places.
      fontSize: 13,
      lineHeight: 20,
      wordWrap: "on" as const,
      renderLineHighlight: editing ? ("line" as const) : ("none" as const),
      automaticLayout: true,
      padding: { top: 8 },
    }),
    [editing],
  );

  if (!editing && blob?.binary && imageMediaTypeForPath(blob.path)) {
    return (
      <ImageViewer
        source={workspaceFileSource(
          client,
          workspaceId,
          blob.path,
          contentRevision,
        )}
        className="bg-page-background min-h-0 flex-1"
      />
    );
  }

  if (!editing && blob?.binary) {
    return (
      <p className="text-muted-foreground px-3 py-6 text-sm">
        This file is binary, so the editor does not open it.
      </p>
    );
  }

  // Editing shows the buffer; reading shows the file as last read.
  const text = draft ? draft.text : (blob?.content ?? "");
  return (
    <>
      {!editing && blob?.truncated && (
        <p className="text-muted-foreground border-b px-3 py-2 text-xs">
          Showing the first part of this file.
        </p>
      )}
      {comparison !== null && draft && (
        <div className="flex min-h-0 flex-1 flex-col">
          <p className="text-muted-foreground border-b px-3 py-1.5 text-xs">
            What your changes do to the version on disk
          </p>
          <div className="min-h-0 flex-1" aria-label="Comparison">
            <DiffEditor
              height="100%"
              language={monacoLanguage(path)}
              theme={monacoTheme(theme)}
              original={comparison}
              modified={draft.text}
              options={{
                readOnly: true,
                originalEditable: false,
                minimap: { enabled: false },
                scrollBeyondLastLine: false,
                fontSize: 13,
                lineHeight: 20,
                automaticLayout: true,
                renderOverviewRuler: false,
                useInlineViewWhenSpaceIsLimited: true,
              }}
            />
          </div>
        </div>
      )}
      {preview && comparison === null && <MarkdownPreview text={text} />}
      {/* The editor stays mounted under a preview or a comparison, so its
          undo history and cursor are where you left them. */}
      <div
        className={cn(
          "min-h-0 flex-1",
          (preview || comparison !== null) && "hidden",
        )}
        tabIndex={editing ? undefined : 0}
        aria-label="File contents"
      >
        <Editor
          height="100%"
          language={monacoLanguage(path)}
          path={path}
          theme={monacoTheme(theme)}
          value={text}
          onChange={editing ? onChange : undefined}
          onMount={(editor) => {
            editorRef.current = editor;
            reveal(revealLine);
          }}
          options={options}
        />
      </div>
    </>
  );
}

/** The file rendered as markdown, with the app's own renderer. */
function MarkdownPreview({ text }: { text: string }) {
  return (
    <div
      className="min-h-0 flex-1 overflow-auto"
      role="document"
      aria-label="Markdown preview"
    >
      <div className="mx-auto max-w-3xl px-6 py-5">
        <MessageMarkdown headingIds>{text}</MessageMarkdown>
      </div>
    </div>
  );
}

const IMAGE_MEDIA_TYPES = new Map([
  ["avif", "image/avif"],
  ["bmp", "image/bmp"],
  ["gif", "image/gif"],
  ["ico", "image/x-icon"],
  ["jpeg", "image/jpeg"],
  ["jpg", "image/jpeg"],
  ["png", "image/png"],
  ["svg", "image/svg+xml"],
  ["webp", "image/webp"],
]);

export function imageMediaTypeForPath(path: string): string | null {
  const extension = path.toLowerCase().split(".").pop();
  return extension ? (IMAGE_MEDIA_TYPES.get(extension) ?? null) : null;
}

function workspaceFileSource(
  client: Pick<ApiClient, "getCodeWorkspaceFile">,
  workspaceId: string,
  path: string,
  contentRevision: number,
): FileBytesSource {
  return {
    id: `${workspaceId}/${path}`,
    cacheKey: `workspace/${workspaceId}/${path}/${contentRevision}`,
    fetch: (signal, onProgress) =>
      client.getCodeWorkspaceFile(workspaceId, path, signal, onProgress),
  };
}
