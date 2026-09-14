import { describe, expect, it } from "vitest";
import {
  clockTime,
  dayLabel,
  duration,
  relative,
  stamp,
  stampUnixSeconds,
} from "./consoleTime";

// A fixed "now" so every assertion below is about the arithmetic rather than
// about when the suite happened to run.
const NOW = new Date("2026-03-04T12:00:00.000Z").getTime();
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

function ago(ms: number): string {
  return new Date(NOW - ms).toISOString();
}

describe("relative", () => {
  it("reads a recent moment as just now rather than 0 minutes ago", () => {
    expect(relative(ago(5_000), NOW)).toBe("just now");
  });

  it("scales the unit with the distance", () => {
    expect(relative(ago(3 * MINUTE), NOW)).toBe("3 minutes ago");
    expect(relative(ago(MINUTE), NOW)).toBe("1 minute ago");
    expect(relative(ago(2 * HOUR), NOW)).toBe("2 hours ago");
    expect(relative(ago(3 * DAY), NOW)).toBe("3 days ago");
    expect(relative(ago(70 * DAY), NOW)).toBe("2 months ago");
    expect(relative(ago(400 * DAY), NOW)).toBe("1 year ago");
  });

  it("reads a future moment forwards — an expiry has not happened yet", () => {
    expect(relative(ago(-30 * MINUTE), NOW)).toBe("in 30 minutes");
  });

  it("returns unparseable text unchanged rather than inventing a date", () => {
    expect(relative("not a timestamp", NOW)).toBe("not a timestamp");
  });
});

describe("duration", () => {
  it("reads a configured bound the way a person would say it", () => {
    expect(duration(45)).toBe("45s");
    expect(duration(90)).toBe("2m");
    expect(duration(3600)).toBe("1h");
    expect(duration(5400)).toBe("1h 30m");
  });
});

describe("dayLabel", () => {
  it("names the days near the present", () => {
    expect(dayLabel(ago(0), NOW)).toBe("Today");
    expect(dayLabel(ago(DAY), NOW)).toBe("Yesterday");
  });

  it("dates the rest, and adds the year once it stops being this one", () => {
    // A long-running sandbox's earliest events are otherwise
    // indistinguishable from the same date a year ago.
    const thisYear = new Date(NOW - 40 * DAY);
    expect(dayLabel(thisYear.toISOString(), NOW)).not.toMatch(/2026/);
    expect(dayLabel("2025-11-02T09:00:00.000Z", NOW)).toMatch(/2025$/);
  });
});

describe("stamp", () => {
  it("renders an absolute moment, and a unix-seconds one the same way", () => {
    const iso = new Date(2026, 2, 4, 9, 5).toISOString();
    expect(stamp(iso)).toBe("Mar 4, 09:05");
    expect(stampUnixSeconds(Math.floor(new Date(iso).getTime() / 1000))).toBe(
      "Mar 4, 09:05",
    );
  });

  it("renders a timeline row's own time zero-padded", () => {
    expect(clockTime(new Date(2026, 2, 4, 9, 5).toISOString())).toBe("09:05");
    expect(clockTime("nonsense")).toBe("");
  });
});
