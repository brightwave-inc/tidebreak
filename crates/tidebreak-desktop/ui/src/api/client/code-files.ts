import type {
  CodeWorkspaceBlob,
  CodeWorkspaceFileSaved,
  CodeWorkspaceFiles,
  CodeWorkspaceSearch,
  CodeWorkspaceTree,
  FileDownloadProgress,
  SaveCodeWorkspaceFileBody,
} from "../types";
import {
  parseCodeWorkspaceBlob,
  parseCodeWorkspaceFileSaved,
  parseCodeWorkspaceFiles,
  parseCodeWorkspaceSearch,
  parseCodeWorkspaceTree,
} from "../../code/parsers";
import { type Constructor, HttpCore, requireParsed } from "./http";

/** Workspace tree, search, file listing, blobs, and file saves. */
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

    /**
     * Save one text file from the file viewer.
     *
     * `base_hash` is the `hash` the viewer loaded. When the file moved on
     * disk since, the server answers `409 file_changed`, and the error's body
     * names the hash on disk now as `current_hash`. Writes are one attempt,
     * never retried: the server may have taken the first.
     */
    async saveCodeWorkspaceFile(
      workspaceId: string,
      body: SaveCodeWorkspaceFileBody,
    ): Promise<CodeWorkspaceFileSaved> {
      return requireParsed(
        parseCodeWorkspaceFileSaved(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/file`,
            {
              method: "PUT",
              headers: this.headers(true),
              body: JSON.stringify(body),
            },
          ),
        ),
        "code workspace file save",
      );
    }

    async getCodeWorkspaceFile(
      workspaceId: string,
      path: string,
      signal?: AbortSignal,
      onProgress?: (progress: FileDownloadProgress) => void,
    ): Promise<{ bytes: Uint8Array; contentType: string | null }> {
      const params = new URLSearchParams({ path });
      return this.streamBytes(
        `/code/workspaces/${encodeURIComponent(workspaceId)}/file?${params}`,
        signal,
        onProgress,
      );
    }
  };
}
