import type { Project, ProjectDocumentPage } from "../types";
import { type Constructor, HttpCore } from "./http";

/** Projects and their documents. */
export function withProjectsApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    createProject(title: string): Promise<Project> {
      return this.json("/projects", {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({ title }),
      });
    }

    listProjects(): Promise<Project[]> {
      return this.json("/projects", { headers: this.headers() });
    }

    patchProjectTitle(
      projectId: string,
      title: string | null,
    ): Promise<Project> {
      return this.json(`/projects/${encodeURIComponent(projectId)}`, {
        method: "PATCH",
        headers: this.headers(true),
        body: JSON.stringify({ title }),
      });
    }

    deleteProject(projectId: string): Promise<void> {
      return this.json(`/projects/${encodeURIComponent(projectId)}`, {
        method: "DELETE",
        headers: this.headers(),
      });
    }

    /** The files this project shares with every conversation filed under it. */
    listProjectDocuments(projectId: string): Promise<ProjectDocumentPage> {
      return this.json(`/projects/${encodeURIComponent(projectId)}/documents`, {
        headers: this.headers(),
      });
    }

    /**
     * Share one conversation's file with the project that conversation belongs to.
     *
     * The conversation keeps its own copy — a document's owner is part of its id,
     * so the project's is a different document and the transcript that referred to
     * the original still resolves.
     */
    promoteDocumentToProject(
      projectId: string,
      chatId: string,
      documentId: string,
    ): Promise<{ document_id: string }> {
      return this.json(
        `/projects/${encodeURIComponent(projectId)}/documents/promote`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify({ chat_id: chatId, document_id: documentId }),
        },
      );
    }

    deleteProjectDocument(
      projectId: string,
      documentId: string,
    ): Promise<void> {
      return this.json(
        `/projects/${encodeURIComponent(projectId)}/documents/${encodeURIComponent(documentId)}`,
        { method: "DELETE", headers: this.headers() },
      );
    }
  };
}
