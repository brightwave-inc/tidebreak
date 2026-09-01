import type {
  CodeWorkspaceBlob,
  CodeWorkspaceFiles,
  CodeWorkspaceSearch,
  CodeWorkspaceTree,
} from "../types";
import {
  parseCodeWorkspaceBlob,
  parseCodeWorkspaceFiles,
  parseCodeWorkspaceSearch,
  parseCodeWorkspaceTree,
} from "../../code/parsers";
import { type Constructor, HttpCore, requireParsed } from "./http";

/** Workspace tree, search, file listing, and blobs. */
export function withCodeFilesApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    async listCodeWorkspaceTree(
      workspaceId: string,
      query?: { query?: string; limit?: number },
    ): Promise<CodeWorkspaceTree> {
      const params = new URLSearchParams();
      if (query?.query) params.set("query", query.query);
      if (query?.limit !== undefined) params.set("limit", String(query.limit));
      const suffix = params.size > 0 ? `?${params}` : "";
      return requireParsed(
        parseCodeWorkspaceTree(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/tree${suffix}`,
            { headers: this.headers() },
          ),
        ),
        "code workspace tree",
      );
    }

    async searchCodeWorkspace(
      workspaceId: string,
      query: {
        query: string;
        include?: string;
        exclude?: string;
        limit?: number;
        history?: boolean;
      },
    ): Promise<CodeWorkspaceSearch> {
      const params = new URLSearchParams({ query: query.query });
      if (query.include) params.set("include", query.include);
      if (query.exclude) params.set("exclude", query.exclude);
      if (query.limit !== undefined) params.set("limit", String(query.limit));
      if (query.history) params.set("history", "true");
      return requireParsed(
        parseCodeWorkspaceSearch(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/search?${params}`,
            { headers: this.headers() },
          ),
        ),
        "code workspace search",
      );
    }

    async listCodeWorkspaceFiles(
      workspaceId: string,
      turnId?: string,
    ): Promise<CodeWorkspaceFiles> {
      const query = turnId ? `?turn=${encodeURIComponent(turnId)}` : "";
      return requireParsed(
        parseCodeWorkspaceFiles(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/files${query}`,
            { headers: this.headers() },
          ),
        ),
        "code workspace files",
      );
    }

    async getCodeWorkspaceBlob(
      workspaceId: string,
      path: string,
    ): Promise<CodeWorkspaceBlob> {
      const params = new URLSearchParams({ path });
      return requireParsed(
        parseCodeWorkspaceBlob(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/blob?${params}`,
            { headers: this.headers() },
          ),
        ),
        "code workspace blob",
      );
    }
  };
}
