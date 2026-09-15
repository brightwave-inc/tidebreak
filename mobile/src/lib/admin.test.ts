import { describe, expect, it } from "vitest";
import {
  adminFromUsageScope,
  isAuditFailure,
  ledgerCursor,
  ownerDirectory,
  ownerLabel,
  recentAuditFailures,
  withNextAuditPage,
} from "./admin";
import type { AuditEvent, AuditEventPage } from "./gatewayAdmin";

const HOUR_MS = 60 * 60 * 1000;
const DAY_MS = 24 * HOUR_MS;
const NOW = Date.parse("2026-09-15T12:00:00.000Z");

function event(over: Partial<AuditEvent> = {}): AuditEvent {
  return {
    action: "models.provider_created",
    actor: "ada@example.test",
    id: "0199a0d1-0000-7000-8000-000000000001",
    last_occurred_at: "2026-09-15T11:00:00.000Z",
    metadata: {},
    occurred_at: "2026-09-15T11:00:00.000Z",
    occurrence_count: 1,
    outcome: "succeeded",
    target_type: "model_provider",
    ...over,
  };
}

describe("adminFromUsageScope", () => {
  it("reads an installation-wide usage read as administrator", () => {
    expect(adminFromUsageScope("installation")).toBe(true);
  });

  it("reads a self-narrowed usage read as member", () => {
    expect(adminFromUsageScope("self")).toBe(false);
  });

  it("answers nothing for a scope that is about a different question", () => {
    // `filtered` is what a parameterised read carries — it says the server
    // applied the caller's filter, not who the caller is. Promoting or
    // demoting on it would flap the whole administration group whenever the
    // activity screen's filtered read landed first.
    expect(adminFromUsageScope("filtered")).toBeUndefined();
    // An older gateway that never sends the field, and anything unrecognised,
    // leave whatever is cached alone rather than demoting an administrator.
    expect(adminFromUsageScope(undefined)).toBeUndefined();
    expect(adminFromUsageScope(null)).toBeUndefined();
    expect(adminFromUsageScope("")).toBeUndefined();
    expect(adminFromUsageScope("INSTALLATION")).toBeUndefined();
  });
});

describe("withNextAuditPage", () => {
  it("appends the cursor a page handed back", () => {
    expect(withNextAuditPage([null], "c1")).toEqual([null, "c1"]);
    expect(withNextAuditPage([null, "c1"], "c2")).toEqual([null, "c1", "c2"]);
  });

  it("stops at the end of the ledger", () => {
    expect(withNextAuditPage([null, "c1"], null)).toEqual([null, "c1"]);
  });

  it("refuses a cursor already loaded", () => {
    // A gateway that echoed the cursor it was handed would otherwise let every
    // tap stack another query on the same key: the same page rendered again,
    // and a "load more" that never ends.
    expect(withNextAuditPage([null, "c1"], "c1")).toEqual([null, "c1"]);
  });

  it("never mutates the list it was given", () => {
    const cursors = [null, "c1"];
    expect(withNextAuditPage(cursors, "c2")).not.toBe(cursors);
    expect(cursors).toEqual([null, "c1"]);
  });
});

describe("ledgerCursor", () => {
  function page(next?: string | null): AuditEventPage {
    return { data: [event()], ...(next === undefined ? {} : { next_cursor: next }) };
  }

  it("takes the cursor from the newest page loaded, not the first", () => {
    expect(ledgerCursor([page("c1"), page("c2")])).toBe("c2");
  });

  it("is null once the last page carries no cursor", () => {
    expect(ledgerCursor([page("c1"), page(null)])).toBeNull();
    expect(ledgerCursor([page("c1"), page()])).toBeNull();
  });

  it("is null while the last page is still in flight", () => {
    // An undefined page must not resurrect the previous page's cursor: that
    // would offer "load more" for a page already being fetched.
    expect(ledgerCursor([page("c1"), undefined])).toBeNull();
    expect(ledgerCursor([])).toBeNull();
  });
});

describe("audit failures", () => {
  it("counts a denial and a breakage alike, and a success as neither", () => {
    expect(isAuditFailure({ outcome: "denied" })).toBe(true);
    expect(isAuditFailure({ outcome: "failed" })).toBe(true);
    expect(isAuditFailure({ outcome: "succeeded" })).toBe(false);
  });

  it("counts a collapsed row by when it last recurred", () => {
    // Identical actions collapse into one row, so a row first seen last week
    // that recurred an hour ago is something that happened today.
    const recurring = event({
      outcome: "denied",
      occurred_at: "2026-09-08T12:00:00.000Z",
      last_occurred_at: "2026-09-15T11:00:00.000Z",
      occurrence_count: 12,
    });
    expect(recentAuditFailures([recurring], DAY_MS, NOW)).toBe(1);
  });

  it("leaves out failures older than the window and every success", () => {
    const stale = event({
      outcome: "failed",
      occurred_at: "2026-09-01T12:00:00.000Z",
      last_occurred_at: "2026-09-01T12:00:00.000Z",
    });
    const fine = event({ outcome: "succeeded" });
    expect(recentAuditFailures([stale, fine], DAY_MS, NOW)).toBe(0);
  });

  it("excludes an unparseable stamp rather than counting it at the epoch", () => {
    const broken = event({
      outcome: "denied",
      occurred_at: "not-a-time",
      last_occurred_at: "not-a-time",
    });
    expect(recentAuditFailures([broken], DAY_MS, NOW)).toBe(0);
  });

  it("falls back to when the row was first seen if it never recurred", () => {
    const once = event({
      outcome: "denied",
      occurred_at: "2026-09-15T11:00:00.000Z",
      last_occurred_at: "",
    });
    expect(recentAuditFailures([once], DAY_MS, NOW)).toBe(1);
  });
});

describe("owner attribution", () => {
  const directory = ownerDirectory([
    { id: "u1", display_name: "Ada Lovelace", email: "ada@example.test" },
    { id: "u2", display_name: null, email: "grace@example.test" },
    { id: "u3", display_name: null, email: null },
  ]);

  it("prefers a display name and falls back to the email", () => {
    expect(directory.get("u1")).toBe("Ada Lovelace");
    expect(directory.get("u2")).toBe("grace@example.test");
  });

  it("keeps no entry for an account the directory cannot name", () => {
    expect(directory.has("u3")).toBe(false);
  });

  it("says nothing on the reader's own run", () => {
    expect(ownerLabel("u1", directory, "u1")).toBeUndefined();
  });

  it("names every other run, even one the directory missed", () => {
    expect(ownerLabel("u1", directory, "u2")).toBe("Ada Lovelace");
    // Attribution is not best-effort even where the name is: an unnamed owner
    // still tells two people's runs apart, which is the misreading the line
    // exists to prevent.
    expect(ownerLabel("u9ab34cd7-zzz", directory, "u1")).toBe("User u9ab34cd");
  });

  it("labels every row while the viewer's identity is still resolving", () => {
    // Over-attributing one's own run is merely redundant; the failure worth
    // avoiding is somebody else's run reading as yours.
    expect(ownerLabel("u1", directory, undefined)).toBe("Ada Lovelace");
  });
});
