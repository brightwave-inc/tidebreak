import { describe, expect, it } from "vitest";
import { AttachError, discoverMachine } from "./attach";
import { tidebreakMachineResource } from "./resource";

describe("discoverMachine", () => {
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
});
