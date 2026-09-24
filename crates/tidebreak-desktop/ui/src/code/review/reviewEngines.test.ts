import { describe, expect, it } from "vitest";

import type { HarnessCaps, HarnessDoctorEntry } from "../../api/types";
import { harnessDoctor } from "../../stories/fixtures";
import {
  defaultReviewEngine,
  reviewEngineChoices,
  reviewPermissionMode,
  reviewUnavailableReason,
} from "./reviewEngines";

function entry(
  overrides: Partial<HarnessDoctorEntry> & Pick<HarnessDoctorEntry, "kind">,
): HarnessDoctorEntry {
  const base = harnessDoctor.harnesses.find(
    (candidate) => candidate.kind === "claude_code",
  )!;
  return { ...base, authenticated: true, found: true, ...overrides };
}

const planless: HarnessCaps = {
  ...entry({ kind: "grok" }).caps,
  plan_mode: "unsupported",
  structured_approvals: "supported",
};

describe("which engines can review", () => {
  it("reviews in plan mode, else in Ask with every request refused, else not at all", () => {
    expect(reviewPermissionMode(entry({ kind: "codex" }).caps)).toBe("plan");
    expect(reviewPermissionMode(planless)).toBe("ask");
    expect(
      reviewPermissionMode({
        plan_mode: "unknown",
        structured_approvals: "unsupported",
      }),
    ).toBeNull();
  });

  it("says why an engine cannot review now", () => {
    expect(
      reviewUnavailableReason(entry({ kind: "codex", found: false })),
    ).toBe("Not installed");
    expect(
      reviewUnavailableReason(entry({ kind: "codex", authenticated: false })),
    ).toBe("Needs a sign-in");
    // A gateway covers the engine's credentials: its local login is moot.
    expect(
      reviewUnavailableReason(
        entry({
          kind: "codex",
          authenticated: false,
          auth_mode: "gateway_relay",
        }),
      ),
    ).toBeNull();
    expect(
      reviewUnavailableReason(
        entry({
          kind: "opencode",
          caps: {
            ...planless,
            structured_approvals: "unsupported",
          },
        }),
      ),
    ).toBe("Can't review read-only");
    expect(
      reviewEngineChoices([
        entry({ kind: "internal" }),
        entry({ kind: "grok" }),
      ]).map((choice) => choice.entry.kind),
    ).toEqual(["grok"]);
  });
});

describe("the engine a review starts on", () => {
  const ready = reviewEngineChoices([
    entry({ kind: "claude_code" }),
    entry({ kind: "codex" }),
    entry({ kind: "grok", caps: planless }),
  ]);

  it("is never the engine that wrote the changes while another is ready", () => {
    expect(defaultReviewEngine(ready, "claude_code")).toEqual({
      kind: "codex",
      sameAsAuthor: false,
    });
    expect(defaultReviewEngine(ready, "codex")).toEqual({
      kind: "claude_code",
      sameAsAuthor: false,
    });
  });

  it("is the one picked last time, unless that one wrote the changes", () => {
    expect(defaultReviewEngine(ready, "claude_code", "grok")).toEqual({
      kind: "grok",
      sameAsAuthor: false,
    });
    expect(defaultReviewEngine(ready, "grok", "grok")).toEqual({
      kind: "claude_code",
      sameAsAuthor: false,
    });
  });

  it("falls back to the author's own engine only when no other is ready, and says so", () => {
    const alone = reviewEngineChoices([
      entry({ kind: "claude_code" }),
      entry({ kind: "codex", authenticated: false }),
      entry({ kind: "grok", found: false }),
    ]);
    expect(defaultReviewEngine(alone, "claude_code")).toEqual({
      kind: "claude_code",
      sameAsAuthor: true,
    });
    const none = reviewEngineChoices([
      entry({ kind: "codex", authenticated: false }),
    ]);
    expect(defaultReviewEngine(none, "claude_code")).toBeNull();
  });
});
