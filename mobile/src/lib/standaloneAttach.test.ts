import { describe, expect, it, vi } from "vitest";
import {
  AttachError,
  REASON_GATEWAY_MACHINE,
  REASON_LOCAL_ONLY,
  REASON_OIDC_UNSUPPORTED,
  REASON_TOKEN_IS_SERVICE,
  REASON_TOKEN_MALFORMED,
  REASON_TOKEN_REFUSED,
  discoverMachine,
  machineTarget,
} from "./attach";
import { tidebreakMachineResource } from "./resource";
import { attachStandaloneMachine } from "./standaloneAttach";
import { REASON_REQUIRES_TLS, REASON_URL_INVALID } from "./url";

const MACHINE = "https://machine.example.test";
const TOKEN = "a".repeat(64);

/** A transport that answers discovery with `body` and sign-in with `status`. */
function transport(body: unknown, signInStatus = 204) {
  return vi.fn(
    async (url: string, _init: { method?: string; headers?: Record<string, string> }) => {
      if (url.endsWith("/auth/discovery")) {
        return new Response(JSON.stringify(body), { status: 200 });
      }
      return new Response(null, { status: signInStatus });
    },
  );
}

describe("discoverMachine with a machine target", () => {
  it("accepts static_token, the one mode a phone can hold a credential for", async () => {
    const discovered = await discoverMachine(
      MACHINE,
      machineTarget(),
      transport({ mode: "static_token" }),
    );
    // Standalone discovery carries no resource to echo, so the resource is the
    // one this client derived — an internal key, never sent to a gateway.
    expect(discovered.resource).toBe(tidebreakMachineResource(MACHINE));
    expect(discovered.baseUrl).toBe(MACHINE);
    expect(discovered.gatewayUrl).toBeUndefined();
  });

  it("refuses each other mode with its own reason", async () => {
    // Distinct reasons, because each has a different answer for the user: pair
    // the gateway, sign in through the browser, or use the desktop app.
    const cases: [unknown, string][] = [
      [
        { mode: "gateway", gateway_url: "https://gw.example.test", resource: "x" },
        REASON_GATEWAY_MACHINE,
      ],
      [
        { mode: "oidc", issuer_name: "login.example", start_url: `${MACHINE}/auth/oidc/start` },
        REASON_OIDC_UNSUPPORTED,
      ],
      [{ mode: "local" }, REASON_LOCAL_ONLY],
    ];
    for (const [body, reason] of cases) {
      await expect(
        discoverMachine(MACHINE, machineTarget(), transport(body)),
      ).rejects.toMatchObject({ reason, stage: "verify" } satisfies Partial<AttachError>);
    }
  });

  it("refuses a mode it does not know rather than assuming static tokens", async () => {
    await expect(
      discoverMachine(MACHINE, machineTarget(), transport({ mode: "something_new" })),
    ).rejects.toMatchObject({ reason: "not_a_machine" });
  });
});

describe("attachStandaloneMachine", () => {
  it("attaches a static-token machine end to end", async () => {
    const fetchImpl = transport({ mode: "static_token" });
    const stages: string[] = [];
    const machine = await attachStandaloneMachine(MACHINE, TOKEN, {
      fetchImpl,
      onStage: (stage) => stages.push(stage),
    });
    expect(machine).toEqual({
      baseUrl: MACHINE,
      resource: tidebreakMachineResource(MACHINE),
    });
    expect(stages).toEqual(["discover", "verify", "probe"]);
    const signIn = fetchImpl.mock.calls.find(([url]) =>
      url.endsWith("/auth/token-sign-in"),
    );
    expect(signIn?.[0]).toBe(`${MACHINE}/auth/token-sign-in`);
    expect(signIn?.[1]).toMatchObject({
      method: "POST",
      headers: { Authorization: `Bearer ${TOKEN}` },
    });
  });

  it("refuses a plaintext address outside the phone, and says why", async () => {
    const fetchImpl = transport({ mode: "static_token" });
    await expect(
      attachStandaloneMachine("http://machine.example.test", TOKEN, { fetchImpl }),
    ).rejects.toMatchObject({
      reason: REASON_REQUIRES_TLS,
      stage: "validate",
    } satisfies Partial<AttachError>);
    // The refusal is local: nothing was dialed, so no token left the device.
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("still allows loopback, which is the development case", async () => {
    const machine = await attachStandaloneMachine(
      "http://localhost:4321",
      TOKEN,
      { fetchImpl: transport({ mode: "static_token" }) },
    );
    expect(machine.baseUrl).toBe("http://localhost:4321");
  });

  it("keeps url_invalid distinct from the TLS refusal", async () => {
    await expect(
      attachStandaloneMachine("not-a-url", TOKEN, {
        fetchImpl: transport({ mode: "static_token" }),
      }),
    ).rejects.toMatchObject({ reason: REASON_URL_INVALID, stage: "validate" });
  });

  it("rejects a malformed token before it reaches the network", async () => {
    const fetchImpl = transport({ mode: "static_token" });
    await expect(
      attachStandaloneMachine(MACHINE, "too-short", { fetchImpl }),
    ).rejects.toMatchObject({
      reason: REASON_TOKEN_MALFORMED,
      stage: "validate",
    } satisfies Partial<AttachError>);
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("separates a token the machine does not know from one that cannot sign in", async () => {
    await expect(
      attachStandaloneMachine(MACHINE, TOKEN, {
        fetchImpl: transport({ mode: "static_token" }, 401),
      }),
    ).rejects.toMatchObject({ reason: REASON_TOKEN_REFUSED, stage: "probe" });
    // A service principal owns automated sessions and deliberately does not
    // sign in; telling the user to find a different token is the useful answer.
    await expect(
      attachStandaloneMachine(MACHINE, TOKEN, {
        fetchImpl: transport({ mode: "static_token" }, 403),
      }),
    ).rejects.toMatchObject({ reason: REASON_TOKEN_IS_SERVICE, stage: "probe" });
  });

  it("reports a machine too old to offer token sign-in", async () => {
    await expect(
      attachStandaloneMachine(MACHINE, TOKEN, {
        fetchImpl: transport({ mode: "static_token" }, 404),
      }),
    ).rejects.toMatchObject({ reason: "not_a_machine", stage: "probe" });
  });
});
