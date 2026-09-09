import { afterEach, describe, expect, it, vi } from "vitest";
import {
  AttachError,
  DISCOVERY_TIMEOUT_MS,
  REASON_UNREACHABLE,
  discoverMachine,
} from "./attach";
import { tidebreakMachineResource } from "./resource";

describe("discoverMachine", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("accepts a matching echo and rejects a foreign resource", async () => {
    const machine = "https://machine.example.com";
    const gateway = "https://gateway.example.test";
    const derived = tidebreakMachineResource(machine);
    const ok = await discoverMachine(machine, gateway, async () =>
      new Response(
        JSON.stringify({
          mode: "gateway",
          gateway_url: gateway,
          resource: derived,
        }),
        { status: 200 },
      ),
    );
    expect(ok.resource).toBe(derived);

    await expect(
      discoverMachine(machine, gateway, async () =>
        new Response(
          JSON.stringify({
            mode: "gateway",
            gateway_url: gateway,
            resource: "tidebreak:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          }),
          { status: 200 },
        ),
      ),
    ).rejects.toMatchObject({
      reason: "resource_mismatch",
      stage: "verify",
    } satisfies Partial<AttachError>);
  });

  it("times out as unreachable when discovery never answers", async () => {
    vi.useFakeTimers();
    const fetchImpl = vi.fn(
      (_url: string, _init: { signal?: AbortSignal }) =>
        new Promise<Response>(() => {
          // A host behind a VPN drops packets: the transport never settles.
        }),
    );
    const pending = discoverMachine(
      "https://machine.example.com",
      "https://gateway.example.test",
      fetchImpl,
    );
    const assertion = expect(pending).rejects.toMatchObject({
      reason: REASON_UNREACHABLE,
      stage: "discover",
    } satisfies Partial<AttachError>);
    await vi.advanceTimersByTimeAsync(DISCOVERY_TIMEOUT_MS);
    await assertion;
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(fetchImpl.mock.calls[0]?.[1]?.signal?.aborted).toBe(true);
  });

  it("keeps the validation error for a reachable machine with a bad echo", async () => {
    vi.useFakeTimers();
    const pending = discoverMachine(
      "https://machine.example.com",
      "https://gateway.example.test",
      async () =>
        new Response(
          JSON.stringify({
            mode: "gateway",
            gateway_url: "https://gateway.example.test",
            resource:
              "tidebreak:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          }),
          { status: 200 },
        ),
    );
    await expect(pending).rejects.toMatchObject({
      reason: "resource_mismatch",
      stage: "verify",
    } satisfies Partial<AttachError>);
  });
});
