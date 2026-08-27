import { describe, expect, it } from "vitest";

import { monitorSince, nextMonitorDelayMs } from "./CodeDeliveryMonitor";

describe("nextMonitorDelayMs", () => {
  it("has no safety or hidden poll clock", () => {
    expect(nextMonitorDelayMs({ rerunRequested: false })).toBeNull();
  });

  it("reruns immediately when a pass was skipped because one was already running", () => {
    expect(nextMonitorDelayMs({ rerunRequested: true })).toBe(0);
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
