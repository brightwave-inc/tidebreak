import type { CodeWorkspaceSnapshot } from "../types";
import { parseCodeWorkspace } from "../../code/parsers";
import { type Constructor, HttpCore, parseList, requireParsed } from "./http";

/** Code workspaces and their storage lifecycle. */
export function withCodeWorkspacesApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    async listCodeWorkspaces(
      repoId?: string,
    ): Promise<CodeWorkspaceSnapshot[]> {
      const query = repoId ? `?repo_id=${encodeURIComponent(repoId)}` : "";
      const body = await this.json<unknown>(`/code/workspaces${query}`, {
        headers: this.headers(),
      });
      return parseList(body, parseCodeWorkspace, "code workspaces");
    }

    async createCodeWorkspace(body: {
      repo_id: string;
      title?: string;
      base_ref?: string;
    }): Promise<CodeWorkspaceSnapshot> {
      return requireParsed(
        parseCodeWorkspace(
          await this.json("/code/workspaces", {
            method: "POST",
            headers: this.headers(true),
            body: JSON.stringify(body),
          }),
        ),
        "code workspace",
      );
    }

    async getCodeWorkspace(
      workspaceId: string,
    ): Promise<CodeWorkspaceSnapshot> {
      return requireParsed(
        parseCodeWorkspace(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}`,
            {
              headers: this.headers(),
            },
          ),
        ),
        "code workspace",
      );
    }

    async patchCodeWorkspace(
      workspaceId: string,
      body: { title: string },
    ): Promise<CodeWorkspaceSnapshot> {
      return requireParsed(
        parseCodeWorkspace(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}`,
            {
              method: "PATCH",
              headers: this.headers(true),
              body: JSON.stringify(body),
            },
          ),
        ),
        "code workspace",
      );
    }

    async archiveCodeWorkspace(
      workspaceId: string,
      force = false,
    ): Promise<CodeWorkspaceSnapshot> {
      return requireParsed(
        parseCodeWorkspace(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/archive`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify({ force }),
            },
          ),
        ),
        "code workspace",
      );
    }

    /**
     * Reactivate an archived workspace: worktree back at its path, on its kept
     * branch. 409 kinds `branch_missing` and `worktree_path_occupied` mean a
     * true restore is impossible; the caller offers the new-workspace fallback.
     */
    async restoreCodeWorkspace(
      workspaceId: string,
    ): Promise<CodeWorkspaceSnapshot> {
      return requireParsed(
        parseCodeWorkspace(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/restore`,
            {
              method: "POST",
              headers: this.headers(true),
            },
          ),
        ),
        "code workspace",
      );
    }

    /**
     * Run the repo's setup script again on a workspace that is `setup_failed`.
     * The worktree is the one it already has, so a success takes the workspace
     * Active without cutting a second checkout. A second failure comes back as
     * 422 `setup_failed`.
     */
    async retryCodeWorkspaceSetup(
      workspaceId: string,
    ): Promise<CodeWorkspaceSnapshot> {
      return requireParsed(
        parseCodeWorkspace(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/retry-setup`,
            {
              method: "POST",
              headers: this.headers(true),
            },
          ),
        ),
        "code workspace",
      );
    }
  };
}
