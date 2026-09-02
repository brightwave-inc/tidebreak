import {
  parseMemoryCaps,
  parseMemoryDigest,
  parseMemoryRecord,
  parseMemoryRevision,
  parseMemorySearchHit,
} from "../parsers";
import type {
  MemoryAuthor,
  MemoryCaps,
  MemoryDigest,
  MemoryEvidence,
  MemoryKind,
  MemoryLink,
  MemoryOrigin,
  MemoryRecord,
  MemoryRecordId,
  MemoryRevision,
  MemoryScope,
  MemorySearchHit,
  MemoryStatus,
} from "../types";
import { type Constructor, HttpCore, parseList, requireParsed } from "./http";

/** The owner-scoped `/memory` route family: records, revisions, search, and digest. */
export function withMemoryApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    async getMemoryCaps(): Promise<MemoryCaps> {
      return requireParsed(
        parseMemoryCaps(
          await this.json("/memory/capabilities", { headers: this.headers() }),
        ),
        "memory capabilities",
      );
    }

    async listMemoryRecords(): Promise<MemoryRecord[]> {
      const body = await this.json<unknown>("/memory/records", {
        headers: this.headers(),
      });
      return parseList(body, parseMemoryRecord, "memory records");
    }

    async getMemoryRecord(recordId: MemoryRecordId): Promise<MemoryRecord> {
      return requireParsed(
        parseMemoryRecord(
          await this.json(`/memory/records/${encodeURIComponent(recordId)}`, {
            headers: this.headers(),
          }),
        ),
        "memory record",
      );
    }

    async createMemoryRecord(
      scope: MemoryScope,
      body: {
        id: MemoryRecordId;
        kind: MemoryKind;
        status: MemoryStatus;
        title: string;
        body: string;
        author: MemoryAuthor;
        origin?: MemoryOrigin;
        evidence?: MemoryEvidence[];
        links?: MemoryLink[];
        expires_at?: string | null;
        observation_count?: number;
      },
    ): Promise<MemoryRecord> {
      const params = new URLSearchParams();
      if (scope.kind === "repo") params.set("repo_id", scope.repo_id);
      const suffix = params.size > 0 ? `?${params}` : "";
      return requireParsed(
        parseMemoryRecord(
          await this.json(`/memory/records${suffix}`, {
            method: "POST",
            headers: this.headers(true),
            body: JSON.stringify({
              ...body,
              origin: body.origin ?? {
                chat_id: null,
                turn_id: null,
                code_session_id: null,
                code_turn_id: null,
                workspace_id: null,
              },
              evidence: body.evidence ?? [],
              links: body.links ?? [],
              expires_at: body.expires_at ?? null,
              observation_count: body.observation_count ?? 0,
            }),
          }),
        ),
        "memory record",
      );
    }

    async updateMemoryRecord(
      recordId: MemoryRecordId,
      body: {
        expected_revision: number;
        kind: MemoryKind;
        title: string;
        body: string;
        author: MemoryAuthor;
        origin: MemoryOrigin;
        evidence: MemoryEvidence[];
        links: MemoryLink[];
        expires_at: string | null;
        observation_count: number;
      },
    ): Promise<MemoryRecord> {
      return requireParsed(
        parseMemoryRecord(
          await this.json(`/memory/records/${encodeURIComponent(recordId)}`, {
            method: "PATCH",
            headers: this.headers(true),
            body: JSON.stringify(body),
          }),
        ),
        "memory record",
      );
    }

    async setMemoryRecordStatus(
      recordId: MemoryRecordId,
      body: { expected_revision: number; status: MemoryStatus },
    ): Promise<MemoryRecord> {
      return requireParsed(
        parseMemoryRecord(
          await this.json(
            `/memory/records/${encodeURIComponent(recordId)}/status`,
            {
              method: "PUT",
              headers: this.headers(true),
              body: JSON.stringify(body),
            },
          ),
        ),
        "memory record",
      );
    }

    async deleteMemoryRecord(recordId: MemoryRecordId): Promise<void> {
      await this.json(`/memory/records/${encodeURIComponent(recordId)}`, {
        method: "DELETE",
        headers: this.headers(),
      });
    }

    async searchMemory(
      query: string,
      options?: {
        scope?: MemoryScope;
        statuses?: MemoryStatus[];
        limit?: number;
      },
    ): Promise<MemorySearchHit[]> {
      const params = new URLSearchParams({ query });
      if (options?.scope?.kind === "repo") {
        params.set("repo_id", options.scope.repo_id);
      } else if (options?.scope?.kind === "personal") {
        params.set("scope_kind", "personal");
      }
      for (const status of options?.statuses ?? [])
        params.append("statuses", status);
      if (options?.limit != null) params.set("limit", String(options.limit));
      const body = await this.json<unknown>(`/memory/search?${params}`, {
        headers: this.headers(),
      });
      return parseList(body, parseMemorySearchHit, "memory search results");
    }

    async getMemoryDigest(scope: MemoryScope): Promise<MemoryDigest> {
      const params = new URLSearchParams();
      if (scope.kind === "repo") params.set("repo_id", scope.repo_id);
      return requireParsed(
        parseMemoryDigest(
          await this.json(`/memory/digest?${params}`, {
            headers: this.headers(),
          }),
        ),
        "memory digest",
      );
    }

    async getMemoryRevisions(
      recordId: MemoryRecordId,
    ): Promise<MemoryRevision[]> {
      const body = await this.json<unknown>(
        `/memory/records/${encodeURIComponent(recordId)}/revisions`,
        { headers: this.headers() },
      );
      return parseList(body, parseMemoryRevision, "memory revisions");
    }
  };
}
