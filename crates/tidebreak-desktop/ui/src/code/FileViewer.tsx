import { useCallback, useEffect } from "react";
import Editor from "@monaco-editor/react";

import type { ApiClient } from "../api/client";
import type { CodeWorkspaceBlob } from "../api/types";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { configureMonaco, monacoLanguage, monacoTheme } from "./monacoEnv";
import { useLiveResource } from "./useLiveContent";

configureMonaco();

/**
 * Read-only Monaco view of one worktree file.
 */
export function FileViewer({
  client,
  workspaceId,
  path,
  contentRevision = 0,
}: {
  client: Pick<ApiClient, "getCodeWorkspaceBlob">;
  workspaceId: string;
  path: string;
  contentRevision?: number;
}) {
  const load = useCallback(
    () => client.getCodeWorkspaceBlob(workspaceId, path),
    [client, workspaceId, path],
  );
  const {
    data,
    error,
    refreshing,
  } = useLiveResource({
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
      <header className="flex shrink-0 items-center justify-between gap-2 border-b px-3 py-2">
        <p className="min-w-0 truncate font-mono text-xs" title={path}>
          {path}
        </p>
        {refreshing && <Spinner className="size-3.5" aria-label="Refreshing" />}
      </header>
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      {!data && !error && (
        <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
          <Skeleton className="h-4 w-1/3" />
          <Skeleton className="h-4 w-3/4" />
          <Skeleton className="h-4 w-2/3" />
        </div>
      )}
      {data && <BlobBody blob={data} />}
    </div>
  );
}

function BlobBody({ blob }: { blob: CodeWorkspaceBlob }) {
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
          theme={monacoTheme()}
          value={blob.content}
          options={{
            readOnly: true,
            minimap: { enabled: false },
            scrollBeyondLastLine: false,
            fontSize: 13,
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
