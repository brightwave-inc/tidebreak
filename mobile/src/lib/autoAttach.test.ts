import { describe, expect, it, vi } from "vitest";
import {
  advertisedMachineUrl,
  attachFailureFromParams,
  attachFailureParams,
  autoAttach,
  describeAttachFailure,
} from "./autoAttach";
import { AttachError, REASON_UNREACHABLE } from "./attach";
import { tidebreakMachineResource } from "./resource";
import type { HttpFetch, HttpResponse } from "./http";

const GATEWAY = "https://gateway.example.test";
const MACHINE = "https://machine.example.com";

/** Discovery + `/policy` answered the way a healthy paired machine answers. */
function healthyMachine(overrides: { resource?: string; gatewayUrl?: string } = {}) {
  const calls: string[] = [];
  const fetchImpl: HttpFetch = async (url) => {
    calls.push(url);
    if (url.endsWith("/auth/discovery")) {
      return new Response(
        JSON.stringify({
          mode: "gateway",
          gateway_url: overrides.gatewayUrl ?? GATEWAY,
          resource: overrides.resource ?? tidebreakMachineResource(MACHINE),
        }),
        { status: 200 },
      ) as unknown as HttpResponse;
    }
    return new Response("{}", { status: 200 }) as unknown as HttpResponse;
  };
  return { fetchImpl, calls };
}

describe("advertisedMachineUrl", () => {
  it("treats a missing, null, or blank machine URL as nothing to attach", () => {
    expect(advertisedMachineUrl(null)).toBeNull();
    expect(advertisedMachineUrl({})).toBeNull();
    expect(advertisedMachineUrl({ machinePrefillUrl: "   " })).toBeNull();
    expect(advertisedMachineUrl({ machinePrefillUrl: ` ${MACHINE} ` })).toBe(
      MACHINE,
    );
  });
});

describe("autoAttach", () => {
  it("attaches the advertised machine without a confirm step", async () => {
    const { fetchImpl, calls } = healthyMachine();
    const getAccessToken = vi.fn(async () => "machine-token");
    const outcome = await autoAttach(
      { gatewayUrl: GATEWAY, machinePrefillUrl: MACHINE },
      { getAccessToken, fetchImpl },
    );
    expect(outcome).toEqual({
      kind: "attached",
      machine: {
        baseUrl: MACHINE,
        resource: tidebreakMachineResource(MACHINE),
      },
    });
    // The same validation the Attach screen runs, in the same order.
    expect(calls).toEqual([
      `${MACHINE}/auth/discovery`,
      `${MACHINE}/policy`,
    ]);
    expect(getAccessToken).toHaveBeenCalledWith(tidebreakMachineResource(MACHINE));
  });

  it("falls back with no failure when the gateway advertised no machine", async () => {
    const getAccessToken = vi.fn(async () => "machine-token");
    const fetchImpl = vi.fn();
    const outcome = await autoAttach(
      { gatewayUrl: GATEWAY },
      { getAccessToken, fetchImpl: fetchImpl as unknown as HttpFetch },
    );
    expect(outcome).toEqual({ kind: "manual", failure: null });
    expect(fetchImpl).not.toHaveBeenCalled();
    expect(getAccessToken).not.toHaveBeenCalled();
  });

  it("falls back to the screen's error state when validation refuses the machine", async () => {
    const { fetchImpl } = healthyMachine({
      resource:
        "tidebreak:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    });
    const outcome = await autoAttach(
      { gatewayUrl: GATEWAY, machinePrefillUrl: MACHINE },
      { getAccessToken: async () => "machine-token", fetchImpl },
    );
    expect(outcome.kind).toBe("manual");
    expect(outcome).toMatchObject({
      failure: { kind: "error" },
    });
  });

  it("falls back to the unreachable state when discovery times out", async () => {
    vi.useFakeTimers();
    try {
      const fetchImpl = (() =>
        new Promise<HttpResponse>(() => {
          // A machine behind a VPN drops packets; the transport never settles.
        })) as unknown as HttpFetch;
      const pending = autoAttach(
        { gatewayUrl: GATEWAY, machinePrefillUrl: MACHINE },
        { getAccessToken: async () => "machine-token", fetchImpl },
      );
      await vi.advanceTimersByTimeAsync(10_000);
      const outcome = await pending;
      // Never throws: a dead spinner is the one outcome the caller cannot show.
      expect(outcome).toMatchObject({
        kind: "manual",
        failure: { kind: "unreachable" },
      });
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("attach failure round-trip", () => {
  it("carries the unreachable state to the Attach screen's route params", () => {
    const failure = describeAttachFailure(
      new AttachError("discover", REASON_UNREACHABLE, "No response after 10 seconds."),
    );
    expect(failure).toEqual({
      kind: "unreachable",
      detail: "No response after 10 seconds.",
    });
    expect(attachFailureFromParams(attachFailureParams(failure))).toEqual(
      failure,
    );
  });

  it("carries a validation failure with its stage prefix", () => {
    const failure = describeAttachFailure(
      new AttachError("verify", "resource_mismatch", "Refusing to attach."),
    );
    expect(failure).toEqual({
      kind: "error",
      message: "verify: Refusing to attach.",
    });
    expect(attachFailureFromParams(attachFailureParams(failure))).toEqual(
      failure,
    );
  });

  it("reads no failure from a plain visit to the Attach screen", () => {
    expect(attachFailureFromParams({})).toBeNull();
    expect(attachFailureFromParams({ failure: "nonsense" })).toBeNull();
    // expo-router hands back an array when a param repeats.
    expect(
      attachFailureFromParams({ failure: ["error"], detail: ["boom"] }),
    ).toEqual({ kind: "error", message: "boom" });
  });

  it("describes a non-AttachError as a plain message", () => {
    expect(describeAttachFailure(new Error("network down"))).toEqual({
      kind: "error",
      message: "network down",
    });
    expect(describeAttachFailure("nope")).toEqual({
      kind: "error",
      message: "Attach failed.",
    });
  });
});
