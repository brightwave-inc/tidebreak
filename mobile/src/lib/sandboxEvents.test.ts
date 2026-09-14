import { describe, expect, it } from "vitest";
import {
  appConnectionChip,
  failureReasonChip,
  phaseChip,
  sharedAppStatusChip,
  subscriptionLimitChip,
} from "./consoleLabels";
import { describeEvent, HIDDEN_EVENT_KINDS } from "./sandboxEvents";
import type { SandboxEvent } from "./consoleTypes";

function event(over: Partial<SandboxEvent> = {}): SandboxEvent {
  return {
    seq: 1,
    kind: "spawned",
    payload: {},
    created_at: "2026-03-04T09:00:00.000Z",
    ...over,
  };
}

describe("describeEvent", () => {
  it("renders a one-liner rather than a kind and a payload dump", () => {
    expect(describeEvent(event({ kind: "spawned", payload: { harness: "codex" } })))
      .toBe("Spawned (codex)");
    expect(
      describeEvent(
        event({ kind: "turn_completed", payload: { turn: 3, exit_code: 0 } }),
      ),
    ).toBe("Turn 3 finished");
    expect(
      describeEvent(
        event({ kind: "turn_completed", payload: { turn: 3, exit_code: 2 } }),
      ),
    ).toBe("Turn 3 exited with code 2");
  });

  it("never renders a blank row for a kind this build has not seen", () => {
    expect(describeEvent(event({ kind: "quantum_entangled" }))).toBe(
      "quantum entangled",
    );
  });

  it("tolerates a payload whose fields are missing or the wrong type", () => {
    expect(
      describeEvent(event({ kind: "turn_started", payload: { turn: "three" } })),
    ).toBe("Turn ? started");
    expect(describeEvent(event({ kind: "pod_lost", payload: {} }))).toBe(
      "Pod lost — infrastructure",
    );
  });

  it("hides only the accounting ticks the meters already carry", () => {
    expect(HIDDEN_EVENT_KINDS.has("spend")).toBe(true);
    expect(HIDDEN_EVENT_KINDS.has("daily_budget")).toBe(true);
    expect(HIDDEN_EVENT_KINDS.has("turn_started")).toBe(false);
  });
});

describe("chips", () => {
  it("reserves the critical tone for malfunction", () => {
    // A bound the installation configured being reached is not a breakage.
    expect(failureReasonChip("oom_killed").tone).toBe("critical");
    expect(failureReasonChip("spend_ceiling_exceeded").tone).toBe("warning");
    expect(failureReasonChip("wall_clock_ceiling").tone).toBe("warning");
    expect(failureReasonChip("grant_revoked").tone).toBe("neutral");
    expect(phaseChip("ceiling_exceeded").tone).toBe("warning");
    expect(phaseChip("failed").tone).toBe("critical");
  });

  it("shows an unknown slug rather than flattening it into a claim", () => {
    expect(phaseChip("defrosting")).toEqual({
      label: "defrosting",
      tone: "neutral",
    });
    expect(failureReasonChip("sun_exploded").label).toBe("sun exploded");
    expect(appConnectionChip("half_connected")).toEqual({
      label: "half connected",
      tone: "neutral",
    });
    expect(sharedAppStatusChip("archived").label).toBe("archived");
    expect(subscriptionLimitChip("suspended").label).toBe("suspended");
  });

  it("badges a phase even when the gateway sent none", () => {
    expect(phaseChip(undefined).label).toBe("Pending");
  });
});
