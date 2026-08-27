import { useCallback, useEffect, useRef } from "react";
import Editor, { type OnMount } from "@monaco-editor/react";

import type { ApiClient } from "../api/client";
import type { CodeWorkspaceBlob } from "../api/types";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { useTheme } from "@/theme";
import { configureMonaco, monacoLanguage, monacoTheme } from "./monacoEnv";
import { MiddleTruncate } from "./MiddleTruncate";
import { OpenInEditorButton } from "./OpenInEditorButton";
import { useLiveResource } from "./useLiveContent";

/**
 * Read-only Monaco view of one worktree file.
 */
export function FileViewer({
  client,
  workspaceId,
  path,
  contentRevision = 0,
  revealLine,
  revealRevision = 0,
  onOpenInEditor,
}: {
  client: Pick<ApiClient, "getCodeWorkspaceBlob">;
  workspaceId: string;
  path: string;
  contentRevision?: number;
  revealLine?: number;
  revealRevision?: number;
  /** Hand this file, at the line on screen, to the reader's own editor. */
  onOpenInEditor?: (path: string, line?: number) => void;
}) {
  const { resolved: resolvedTheme } = useTheme();
  const load = useCallback(
    () => client.getCodeWorkspaceBlob(workspaceId, path),
    [client, workspaceId, path],
  );
  const { data, error, refreshing } = useLiveResource({
    key: `${workspaceId}:${path}`,
    revision: contentRevision,
    load,
    errorMessage: "Could not open that file",
  });

  useEffect(() => {
    configureMonaco();
  }, []);

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      {/*
        Deliberately the same two-line header as `DiffPanel`: a heading over a
        mono caption, the same padding, the same fixed spinner slot. The two
        panels share one center tab strip, so any difference in their header
        height shows up as the file body jumping when the reader switches tabs.
      */}
      <header className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b px-3 py-2">
        <div className="min-w-0">
          <h2 className="text-sm font-medium">File</h2>
          <MiddleTruncate
            text={path}
            className="text-muted-foreground font-mono text-xs"
          />
        </div>
        <div className="flex items-center gap-2">
          <span className="grid size-3.5 shrink-0 place-items-center">
            {refreshing && (
              <Spinner className="size-3.5" aria-label="Refreshing" />
            )}
          </span>
          {onOpenInEditor && (
            <OpenInEditorButton
              onClick={() => onOpenInEditor(path, revealLine)}
            />
          )}
        </div>
      </header>
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      {!data && !error && (
        <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
          <Skeleton className="h-4 w-1/3" />
          <Skeleton className="h-4 w-3/4" />
          <Skeleton className="h-4 w-2/3" />
        </div>
      )}
      {data && (
        <BlobBody
          blob={data}
          theme={resolvedTheme}
          revealLine={revealLine}
          revealRevision={revealRevision}
        />
      )}
    </div>
  );
}

function BlobBody({
  blob,
  theme,
  revealLine,
  revealRevision,
}: {
  blob: CodeWorkspaceBlob;
  theme: "light" | "dark";
  revealLine?: number;
  revealRevision: number;
}) {
  const editorRef = useRef<Parameters<OnMount>[0] | null>(null);
  const revealFrameRef = useRef<number | null>(null);

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

  if (blob.binary) {
    return (
      <p className="text-muted-foreground px-3 py-6 text-sm">
        This file is binary, so the editor does not open it.
      </p>
    );
  }
  return (
    <>
      {blob.truncated && (
        <p className="text-muted-foreground border-b px-3 py-2 text-xs">
          Showing the first part of this file.
        </p>
      )}
      <div className="min-h-0 flex-1">
        <Editor
          height="100%"
          language={monacoLanguage(blob.path)}
          path={blob.path}
          theme={monacoTheme(theme)}
          value={blob.content}
          onMount={(editor) => {
            editorRef.current = editor;
            reveal(revealLine);
          }}
          options={{
            readOnly: true,
            minimap: { enabled: false },
            scrollBeyondLastLine: false,
            // The same 13px/20px as the diff body. The two views open the same
            // file from the same tab strip, so a reader flipping between them
            // should see the same lines in the same places.
            fontSize: 13,
            lineHeight: 20,
            wordWrap: "on",
            renderLineHighlight: "none",
            automaticLayout: true,
            padding: { top: 8 },
          }}
        />
      </div>
    </>
  );
}
