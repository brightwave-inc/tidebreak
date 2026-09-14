import { describe, expect, it } from "vitest";
import type {
  SandboxConcurrencyView,
  SandboxView,
} from "./consoleTypes";
import {
  ALL_STATUS_GROUP_KEYS,
  concurrencyOccupancy,
  groupOfState,
  isKnownState,
  matchesFilter,
  matchesStatusSelection,
  needsAttention,
  schedulingPressureNote,
  statesForGroups,
} from "./sandboxStatus";

function sandbox(over: Partial<SandboxView> = {}): SandboxView {
  return {
    id: "sb-1",
    profile_name: "default",
    user_id: "user-1",
    state: "running",
    harness: "claude_code",
    task_prompt: "Fix the flake",
    created_at: "2026-01-01T00:00:00.000Z",
    latest_event_seq: 4,
    pending_messages: 0,
    ...over,
  };
}

function concurrency(
  over: Partial<SandboxConcurrencyView> = {},
): SandboxConcurrencyView {
  return {
    user_limit: 4,
    user_running: 1,
    user_awaiting_scheduling: 0,
    user_exhausted: false,
    installation_limit: 20,
    installation_running: 6,
    installation_awaiting_scheduling: 0,
    installation_exhausted: false,
    ...over,
  };
}

describe("statesForGroups", () => {
  it("asks for every state by omitting the parameter, not by naming nine", () => {
    // Only an unfiltered read can return a state this build has never heard
    // of, because the server can only answer with states the client asked for.
    expect(statesForGroups([...ALL_STATUS_GROUP_KEYS])).toBeNull();
  });

  it("names exactly the states the selected groups cover", () => {
    expect(statesForGroups(["done", "failed"])).toEqual(["completed", "failed"]);
    expect(statesForGroups(["stopped"])).toEqual([
      "cancelled",
      "expired",
      "ceiling_exceeded",
    ]);
  });
});

describe("matchesStatusSelection", () => {
  it("re-applies the filter locally, because the server filter is additive", () => {
    // A gateway older than `states` ignores the parameter and answers
    // unfiltered; the chips would otherwise decorate an unfiltered list.
    expect(matchesStatusSelection("completed", ["live"])).toBe(false);
    expect(matchesStatusSelection("running", ["live"])).toBe(true);
  });

  it("shows a state this build does not know under every selection", () => {
    // The run exists, it is the user's, and no chip can be taught to show it.
    expect(isKnownState("hibernating")).toBe(false);
    expect(matchesStatusSelection("hibernating", ["live"])).toBe(true);
    expect(matchesStatusSelection("hibernating", ["done"])).toBe(true);
  });
});

describe("groupOfState", () => {
  it("counts only states a chip covers", () => {
    expect(groupOfState("running")).toBe("live");
    expect(groupOfState("ceiling_exceeded")).toBe("stopped");
    // Filing a configured bound under Failed would report the system working
    // as configured as though something had broken.
    expect(groupOfState("expired")).toBe("stopped");
    expect(groupOfState("failed")).toBe("failed");
    expect(groupOfState("hibernating")).toBeUndefined();
  });
});

describe("needsAttention", () => {
  it("flags the reader's own parked run", () => {
    expect(
      needsAttention(sandbox({ may_resume: false }), "user-1"),
    ).toBe(true);
  });

  it("stays quiet about a run that resumes on its own", () => {
    expect(needsAttention(sandbox({ may_resume: true }), "user-1")).toBe(false);
    expect(needsAttention(sandbox(), "user-1")).toBe(false);
  });

  it("never claims a terminal run is waiting on anyone", () => {
    expect(
      needsAttention(
        sandbox({ state: "cancelled", may_resume: false }),
        "user-1",
      ),
    ).toBe(false);
  });

  it("refuses to claim a run is waiting on a reader it cannot identify", () => {
    expect(needsAttention(sandbox({ may_resume: false }), undefined)).toBe(
      false,
    );
    expect(needsAttention(sandbox({ may_resume: false }), "someone")).toBe(
      false,
    );
  });
});

describe("concurrencyOccupancy", () => {
  it("gives each total its own waiting count", () => {
    // Your own slots all placed while somebody else's sit unschedulable must
    // not read as though your quota is the wait.
    const line = concurrencyOccupancy(
      concurrency({ installation_awaiting_scheduling: 3 }),
    );
    expect(line).toContain("1 of 4 live for you.");
    expect(line).toContain(
      "6 of 20 live across the installation, 3 of them waiting for a node.",
    );
  });
});

describe("schedulingPressureNote", () => {
  it("says nothing when everything live is placed", () => {
    expect(schedulingPressureNote(concurrency())).toBeUndefined();
  });

  it("reports the cluster's own reason for what it has not placed", () => {
    expect(
      schedulingPressureNote(
        concurrency({
          installation_awaiting_scheduling: 2,
          scheduling_pressure: { reason: "Unschedulable" },
        }),
      ),
    ).toContain("The cluster reports Unschedulable");
  });

  it("names the limiting class and whether autoscaling is helping", () => {
    expect(
      schedulingPressureNote(
        concurrency({
          schedulable: {
            available: 0,
            limiting_class: "ephemeral_storage",
            autoscaling_active: false,
            retry_after_seconds: 30,
          },
        }),
      ),
    ).toBe(
      "No schedulable capacity (ephemeral storage). Autoscaling is not adding capacity. Retry in 30 seconds.",
    );
  });
});

describe("matchesFilter", () => {
  it("matches everything when nothing was typed", () => {
    expect(matchesFilter("   ", "anything")).toBe(true);
  });

  it("matches case-insensitively across the fields given", () => {
    expect(matchesFilter("FLAKE", "Fix the flake", null)).toBe(true);
    expect(matchesFilter("flake", undefined, "default")).toBe(false);
  });
});
