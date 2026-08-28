import { describe, expect, it } from "vitest";

import type { CodeDeliveryRunQuery } from "../api/types";
import {
  monitorRuns,
  monitorSince,
  nextMonitorDelayMs,
} from "./CodeDeliveryMonitor";

describe("nextMonitorDelayMs", () => {
  it("has no safety or hidden poll clock", () => {
    expect(nextMonitorDelayMs({ rerunRequested: false })).toBeNull();
  });

  it("reruns immediately when a pass was skipped because one was already running", () => {
    expect(nextMonitorDelayMs({ rerunRequested: true })).toBe(0);
  });
});

describe("monitorRuns", () => {
  it("asks only for persisted workflow runs", async () => {
    const queries: CodeDeliveryRunQuery[] = [];
    await monitorRuns(
      {
        queryCodeDeliveryRuns: async (query) => {
          queries.push(query);
          return {
            capability: { found: false, remediation: "test" },
            items: [],
            errors: [],
            fetched_at: "2026-08-20T12:00:00.000Z",
          };
        },
      },
      [],
      "2026-08-20T00:00:00.000Z",
    );
    expect(queries).toHaveLength(1);
    expect(queries[0]?.kinds).toEqual(["workflow_run"]);
  });
});

describe("monitorSince", () => {
  const now = Date.parse("2026-08-20T12:00:00.000Z");

  it("looks back 24 hours before the first successful poll", () => {
    expect(monitorSince(null, now)).toBe("2026-08-19T12:00:00.000Z");
    expect(monitorSince("not-a-timestamp", now)).toBe(
      "2026-08-19T12:00:00.000Z",
    );
  });

  it("overlaps later polls by two minutes", () => {
    expect(monitorSince("2026-08-20T11:15:00.000Z", now)).toBe(
      "2026-08-20T11:13:00.000Z",
    );
  });

  it("never asks for more than 30 days of history", () => {
    expect(monitorSince("2026-01-01T00:00:00.000Z", now)).toBe(
      "2026-07-21T12:00:00.000Z",
    );
  });
});
