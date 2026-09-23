import { afterEach, describe, expect, it, vi } from "vitest";
import { API_LEVEL } from "../generated/wire";
import {
  AttachError,
  DISCOVERY_TIMEOUT_MS,
  REASON_APP_TOO_OLD,
  REASON_MACHINE_TOO_OLD,
  REASON_UNREACHABLE,
  discoverMachine,
  gatewayTarget,
  machineTarget,
  requireCompatibleMachine,
} from "./attach";
import { tidebreakMachineResource } from "./resource";

describe("the version handshake", () => {
  const machine = "https://machine.example.com";
  const gateway = "https://gateway.example.test";

  function discovery(extra: Record<string, unknown>) {
    return async () =>
      new Response(
        JSON.stringify({
          mode: "gateway",
          gateway_url: gateway,
          resource: tidebreakMachineResource(machine),
          ...extra,
        }),
        { status: 200 },
      );
  }

  it("refuses a newer machine by name instead of failing on its data later", async () => {
    await expect(
      discoverMachine(
        machine,
        gatewayTarget(gateway),
        discovery({ version: "9.4.0", api_level: API_LEVEL + 1 }),
      ),
    ).rejects.toMatchObject({
      reason: REASON_APP_TOO_OLD,
      message:
        "This machine runs Tidebreak 9.4.0, which is newer than this app supports. Update the app to connect.",
    } satisfies Partial<AttachError>);
  });

  it("attaches a machine at this app's level, and one from before the handshake", async () => {
    await expect(
      discoverMachine(
        machine,
        gatewayTarget(gateway),
        discovery({ version: "1.0.0", api_level: API_LEVEL }),
      ),
    ).resolves.toMatchObject({ baseUrl: machine });
    await expect(
      discoverMachine(machine, gatewayTarget(gateway), discovery({})),
    ).resolves.toMatchObject({ baseUrl: machine });
  });

  it("checks a standalone machine after its mode", async () => {
    await expect(
      discoverMachine(machine, machineTarget(), async () =>
        new Response(
          JSON.stringify({
            mode: "static_token",
            version: "9.4.0",
            api_level: API_LEVEL + 1,
          }),
          { status: 200 },
        ),
      ),
    ).rejects.toMatchObject({ reason: REASON_APP_TOO_OLD });
  });

  it("asks for a machine update when the machine is below the supported range", () => {
    expect(() =>
      requireCompatibleMachine(
        { version: "0.9.0", api_level: 1 },
        { min: 2, max: 3 },
      ),
    ).toThrow(
      "This machine runs Tidebreak 0.9.0, which this app no longer supports. Update the machine to connect.",
    );
    let caught: unknown = null;
    try {
      requireCompatibleMachine({ api_level: 1 }, { min: 2, max: 3 });
    } catch (error) {
      caught = error;
    }
    expect(caught).toMatchObject({ reason: REASON_MACHINE_TOO_OLD });
  });

  it("never puts an unprintable release in a sentence", () => {
    expect(() =>
      requireCompatibleMachine({
        version: "9.4.0\u202e<script>",
        api_level: API_LEVEL + 1,
      }),
    ).toThrow(
      "This machine runs a newer Tidebreak than this app supports. Update the app to connect.",
    );
  });
});

describe("discoverMachine", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("accepts a matching echo and rejects a foreign resource", async () => {
    const machine = "https://machine.example.com";
    const gateway = "https://gateway.example.test";
    const derived = tidebreakMachineResource(machine);
    const ok = await discoverMachine(machine, gatewayTarget(gateway), async () =>
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
      discoverMachine(machine, gatewayTarget(gateway), async () =>
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
      gatewayTarget("https://gateway.example.test"),
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
      gatewayTarget("https://gateway.example.test"),
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
