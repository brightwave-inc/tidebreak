import { Trash2 } from "lucide-react";
import { useEffect, useState } from "react";

import type { ProjectDocument } from "./api";
import { useApp } from "./AppContext";
import { useProjectListStore } from "./ProjectListStore";
import { useConfirm } from "@/components/ConfirmDialog";
import { DocumentIcon } from "@/components/document-table/DocumentIcon";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty";

/** Bytes as a reader reads them: no more precision than the eye needs. */
function fileSize(bytes: number | null): string | null {
  if (bytes === null) return null;
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
}

/**
 * A project's files: the material every conversation filed under it can read.
 *
 * Files arrive here from a conversation rather than from this page. An upload
 * belongs to the conversation it was dropped into until someone decides it is
 * shared, which keeps a project from silently accumulating whatever anyone
 * happened to attach. This page is where that decision is visible and where it
 * is undone.
 */
export function ProjectFilesView({ projectId }: { projectId: string }) {
  const { client } = useApp();
  const projects = useProjectListStore((state) => state.projects);
  const project = projects.find((candidate) => candidate.id === projectId);
  const [documents, setDocuments] = useState<ProjectDocument[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const { confirm, dialog: confirmDialog } = useConfirm();

  useEffect(() => {
    let cancelled = false;
    setDocuments(null);
    setError(null);
    void client
      .listProjectDocuments(projectId)
      .then((page) => {
        if (!cancelled) setDocuments(page.documents);
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [client, projectId]);

  async function remove(document: ProjectDocument) {
    const name = document.title ?? "this file";
    if (
      !(await confirm({
        title: `Remove ${name} from the project?`,
        description:
          "Conversations in this project stop seeing it. The conversation it came from keeps its own copy.",
        confirmLabel: "Remove",
        destructive: true,
      }))
    ) {
      return;
    }
    setRemoving(document.document_id);
    setError(null);
    try {
      await client.deleteProjectDocument(projectId, document.document_id);
      setDocuments(
        (current) =>
          current?.filter(
            (candidate) => candidate.document_id !== document.document_id,
          ) ?? null,
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setRemoving(null);
    }
  }

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-4 p-6">
      <div>
        <h1 className="text-lg font-medium">
          {project?.title?.trim() || "Untitled project"}
        </h1>
        <p className="text-sm text-muted-foreground">
          Files here are readable by every conversation in this project.
        </p>
      </div>

      {error && <p className="text-destructive text-sm">{error}</p>}

      {documents === null && !error && (
        <p className="text-sm text-muted-foreground">Loading files…</p>
      )}

      {documents !== null && documents.length === 0 && (
        <Empty>
          <EmptyHeader>
            <EmptyTitle>No project files</EmptyTitle>
            <EmptyDescription>
              Open a file attached to one of this project's conversations and
              choose “Add to project” to share it with the rest.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      )}

      {documents !== null && documents.length > 0 && (
        <ul className="m-0 flex list-none flex-col gap-2 p-0" aria-label="Project files">
          {documents.map((document) => {
            const size = fileSize(document.source_byte_len);
            return (
              <li
                key={document.document_id}
                className="flex items-center gap-3 rounded-lg border border-border px-3 py-2"
              >
                <DocumentIcon
                  mediaType={document.media_type}
                  className="size-4"
                  aria-hidden="true"
                />
                <span className="min-w-0 flex-1 truncate text-sm">
                  {document.title ?? "Untitled file"}
                </span>
                {size && (
                  <span className="shrink-0 text-xs text-muted-foreground">
                    {size}
                  </span>
                )}
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={`Remove ${document.title ?? "file"} from the project`}
                  disabled={removing !== null}
                  onClick={() => void remove(document)}
                >
                  <Trash2 className="size-4" />
                </Button>
              </li>
            );
          })}
        </ul>
      )}
      {confirmDialog}
    </div>
  );
}
