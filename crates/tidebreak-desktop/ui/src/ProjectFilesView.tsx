import { useRouterState } from "@tanstack/react-router";
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
import { InstructionsField } from "@/settings/InstructionsField";
import { SettingsSection } from "@/settings/primitives";
import { Notice, NoticeRetryButton } from "@/components/ui/notice";
import { friendlyErrorMessage } from "@/lib/utils";

/**
 * The location hash the project menu's Instructions item opens this page
 * with, so the reader can start typing without finding the field first.
 */
export const PROJECT_INSTRUCTIONS_HASH = "instructions";

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
 * A project's page: the instructions and files behind every conversation
 * filed under it.
 *
 * Instructions come first because they shape every reply. Files arrive here
 * from a conversation rather than from this page. An upload belongs to the
 * conversation it was dropped into until someone decides it is shared, which
 * keeps a project from silently accumulating whatever anyone happened to
 * attach. This page is where that decision is visible and where it is undone.
 */
export function ProjectFilesView({ projectId }: { projectId: string }) {
  const { client } = useApp();
  const projects = useProjectListStore((state) => state.projects);
  const projectsLoaded = useProjectListStore((state) => state.projectsLoaded);
  const replaceProject = useProjectListStore((state) => state.replaceProject);
  const project = projects.find((candidate) => candidate.id === projectId);
  const focusInstructions = useRouterState({
    select: (state) => state.location.hash === PROJECT_INSTRUCTIONS_HASH,
  });
  const [documents, setDocuments] = useState<ProjectDocument[] | null>(null);
  /** Why the file list did not load; its Try again runs the load once more. */
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loadAttempt, setLoadAttempt] = useState(0);
  /** Why a removal failed; the list stays as it was. */
  const [removeError, setRemoveError] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const { confirm, dialog: confirmDialog } = useConfirm();

  useEffect(() => {
    let cancelled = false;
    setDocuments(null);
    setLoadError(null);
    setRemoveError(null);
    void client
      .listProjectDocuments(projectId)
      .then((page) => {
        if (!cancelled) setDocuments(page.documents);
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setLoadError(friendlyErrorMessage(err, "Try again in a moment."));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [client, projectId, loadAttempt]);

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
    setRemoveError(null);
    try {
      await client.deleteProjectDocument(projectId, document.document_id);
      setDocuments(
        (current) =>
          current?.filter(
            (candidate) => candidate.document_id !== document.document_id,
          ) ?? null,
      );
    } catch (err) {
      setRemoveError(
        friendlyErrorMessage(err, `Could not remove ${name}. Try again.`),
      );
    } finally {
      setRemoving(null);
    }
  }

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-5 p-6">
      <div>
        <h1 className="text-lg font-medium">
          {project?.title?.trim() || "Untitled project"}
        </h1>
        <p className="text-sm text-muted-foreground">
          Every conversation in this project follows these instructions and can
          read these files.
        </p>
      </div>

      <SettingsSection title="Instructions">
        {project ? (
          <InstructionsField
            key={project.id}
            label="Project instructions"
            hint="Applies to every conversation in this project, after your personal instructions, starting with the next message. Changes save when you leave the field."
            placeholder="This project tracks the 2026 filing. Cite the source document for every figure."
            saved={project.instructions}
            autoFocus={focusInstructions}
            onSave={async (instructions) => {
              replaceProject(
                await client.patchProjectInstructions(project.id, instructions),
              );
            }}
          />
        ) : (
          <p className="text-sm text-muted-foreground" role="status">
            {projectsLoaded
              ? "This project is not available."
              : "Loading instructions…"}
          </p>
        )}
      </SettingsSection>

      <SettingsSection title="Files">
        {loadError && (
          <Notice
            tone="critical"
            title="Could not load the project's files"
            action={
              <NoticeRetryButton
                onClick={() => setLoadAttempt((count) => count + 1)}
              />
            }
          >
            {loadError}
          </Notice>
        )}
        {removeError && <Notice tone="critical">{removeError}</Notice>}

        {documents === null && !loadError && (
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
          <ul
            className="m-0 flex list-none flex-col gap-2 p-0"
            aria-label="Project files"
          >
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
      </SettingsSection>
      {confirmDialog}
    </div>
  );
}
