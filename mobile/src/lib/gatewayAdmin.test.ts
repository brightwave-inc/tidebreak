import { describe, expect, it } from "vitest";

import {
  type AuditEvent,
  type AuditEventPage,
  type AuditEventQuery,
  type Sandbox,
  type SandboxPage,
  nextAuditCursor,
} from "@/lib/gatewayAdmin";

/**
 * Exercises the generated admin types so the committed artifact is more than a
 * file on disk: if a regenerated document drops `next_cursor`, renames `data`,
 * or loses an operation, this stops compiling and the console slice's
 * assumptions have to be revisited rather than discovered on device.
 */

/** Compile-time identity assertion; no runtime cost. */
type Expect<T extends true> = T;
type Equals<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2
  ? true
  : false;

type _PageCarriesEvents = Expect<Equals<AuditEventPage["data"], AuditEvent[]>>;
type _SandboxPageCarriesSandboxes = Expect<Equals<SandboxPage["data"], Sandbox[]>>;

const event: AuditEvent = {
  action: "models.provider_created",
  actor: "ada@example.com",
  id: "0199a0d1-0000-7000-8000-000000000001",
  last_occurred_at: "2026-09-14T10:00:00Z",
  metadata: {},
  occurred_at: "2026-09-14T10:00:00Z",
  occurrence_count: 1,
  outcome: "succeeded",
  target_type: "model_provider",
};

describe("gateway admin types", () => {
  it("reads a cursor off a page that has one", () => {
    const page: AuditEventPage = { data: [event], next_cursor: "opaque" };
    expect(nextAuditCursor(page)).toBe("opaque");
  });

  it("treats an absent and an explicitly null cursor alike", () => {
    expect(nextAuditCursor({ data: [event] })).toBeNull();
    expect(nextAuditCursor({ data: [], next_cursor: null })).toBeNull();
  });

  it("types the audit filters the ledger read accepts", () => {
    const query: AuditEventQuery = { action: "models.", limit: 50, outcome: "succeeded" };
    expect(query.action).toBe("models.");
  });

  it("types an empty sandbox page", () => {
    const page: SandboxPage = { data: [] };
    expect(page.data).toHaveLength(0);
  });
});
