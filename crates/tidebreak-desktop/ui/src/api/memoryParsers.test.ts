import { describe, expect, it } from "vitest";

import {
  parseMemoryCaps,
  parseMemoryDigest,
  parseMemoryRecord,
  parseMemoryRevision,
} from "./parsers";

const record = {
  id: "record-1",
  scope: { kind: "personal" },
  kind: "lesson",
  status: "proposed",
  title: "When changing database migrations",
  body: "Run the migration chain test before publishing.",
  provenance: {
    author: "model",
    origin: {
      chat_id: "chat-1",
      turn_id: null,
      code_session_id: null,
      code_turn_id: null,
      workspace_id: null,
    },
    evidence: [{ kind: "message", message_id: "message-1" }],
  },
  links: [],
  expires_at: null,
  superseded_by: null,
  observation_count: 0,
  revision: 1,
  created_at: "2026-09-01T12:00:00Z",
  updated_at: "2026-09-01T12:00:00Z",
};

describe("memory parsers", () => {
  it("accepts a valid record and preserves its typed provenance", () => {
    expect(parseMemoryRecord(record)).toEqual(record);
  });

  it("rejects a model-authored record without evidence", () => {
    const evidenceLess = structuredClone(record);
    evidenceLess.provenance.evidence = [];
    expect(parseMemoryRecord(evidenceLess)).toBeNull();
  });

  it("rejects unknown fields and invalid lifecycle states", () => {
    const extra = { ...record, future_field: true };
    expect(parseMemoryRecord(extra)).toBeNull();

    const invalidStatus = { ...record, status: "authority" };
    expect(parseMemoryRecord(invalidStatus)).toBeNull();
  });

  it("rejects a caps vector that does not state every capability", () => {
    const caps = {
      extraction: "unsupported",
      lexical_search: "supported",
      semantic_search: "unsupported",
      consolidation: "unsupported",
      context_assembly: "supported",
      revision_history: "supported",
      verified_delete: "supported",
      asynchronous_writes: "unsupported",
      agent_editable_surfaces: "supported",
    };
    expect(parseMemoryCaps(caps)).toEqual(caps);
    expect(
      parseMemoryCaps({ ...caps, asynchronous_writes: undefined }),
    ).toBeNull();
  });

  it("rejects a digest whose byte count does not match its markdown", () => {
    const markdown = "## Tidebreak memory\n";
    const digest = {
      scope: { kind: "personal" },
      markdown,
      byte_len: new TextEncoder().encode(markdown).length,
      byte_cap: 8192,
      record_count: 1,
    };
    expect(parseMemoryDigest(digest)).toEqual(digest);
    expect(
      parseMemoryDigest({ ...digest, byte_len: digest.byte_len + 1 }),
    ).toBeNull();
  });

  it("rejects a revision that belongs to another record", () => {
    const revision = {
      id: "revision-1",
      record_id: record.id,
      ordinal: 1,
      snapshot: record,
      created_at: record.created_at,
    };
    expect(parseMemoryRevision(revision)).toEqual(revision);
    expect(
      parseMemoryRevision({ ...revision, record_id: "record-2" }),
    ).toBeNull();
  });
});
