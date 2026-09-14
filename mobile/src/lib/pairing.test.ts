import { describe, expect, it, vi } from "vitest";
import {
  awaitPairingApproval,
  claimPairingSession,
  deriveMatchCode,
  isPairingPending,
  pairingErrorMessage,
  PairingError,
  pollPairingSession,
} from "./pairing";
import type { HttpFetch, HttpResponse } from "./http";

function jsonResponse(status: number, body: unknown): HttpResponse {
  return {
    status,
    ok: status >= 200 && status < 300,
    redirected: false,
    url: "",
    json: async () => body,
    text: async () => JSON.stringify(body),
  };
}

/** Records the one request made and answers with a canned response. */
function recordingFetch(response: HttpResponse) {
  const calls: { url: string; body: URLSearchParams }[] = [];
  const fetchImpl: HttpFetch = async (url, init) => {
    calls.push({ url, body: new URLSearchParams(init.body ?? "") });
    return response;
  };
  return { calls, fetchImpl };
}

describe("deriveMatchCode", () => {
  // The server derives the same code from the same challenge
  // (`derive_pairing_match_code`, mg cli_auth.rs): first 20 bits of
  // SHA-256(challenge), five per character, over the ambiguity-free alphabet.
  // These vectors are computed from that definition, not from this
  // implementation — a drift in either side must fail here rather than on a
  // device, where it shows up as two screens disagreeing and a user denying a
  // legitimate pairing.
  it.each([
    ["test-challenge", "M5-XM"],
    // The RFC 7636 example code challenge.
    ["E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "BW-W3"],
    ["", "6Q-2N"],
  ])("derives %j as %s", (challenge, expected) => {
    expect(deriveMatchCode(challenge)).toBe(expected);
  });

  it("only ever emits characters from the unambiguous alphabet", () => {
    for (let i = 0; i < 200; i += 1) {
      const code = deriveMatchCode(`challenge-${i}`);
      expect(code).toMatch(/^[ABCDEFGHJKLMNPQRSTUVWXYZ23456789]{2}-[ABCDEFGHJKLMNPQRSTUVWXYZ23456789]{2}$/);
    }
  });
});

describe("claimPairingSession", () => {
  it("names this app as the pairing client", async () => {
    // Omitting client_id would mean `tidewatch` server-side, whose registered
    // redirect schemes are not this app's — the claim would succeed and the
    // later redemption would fail on the redirect match.
    const { calls, fetchImpl } = recordingFetch(
      jsonResponse(200, { status: "claimed", interval: 2 }),
    );
    await claimPairingSession(
      "https://gateway.example",
      {
        sessionCode: "mg_ps_abc",
        codeChallenge: "challenge",
        redirectUri: "tidebreak://callback",
        deviceLabel: "Phone",
      },
      fetchImpl,
    );
    expect(calls[0]?.url).toBe(
      "https://gateway.example/oauth/pairing/claim",
    );
    expect(Object.fromEntries(calls[0]?.body ?? [])).toEqual({
      session_code: "mg_ps_abc",
      code_challenge: "challenge",
      redirect_uri: "tidebreak://callback",
      client_id: "tidebreak-mobile",
      device_label: "Phone",
    });
  });

  it("omits the device label when there is none", async () => {
    const { calls, fetchImpl } = recordingFetch(jsonResponse(200, {}));
    await claimPairingSession(
      "https://gateway.example",
      {
        sessionCode: "mg_ps_abc",
        codeChallenge: "challenge",
        redirectUri: "tidebreak://callback",
      },
      fetchImpl,
    );
    expect(calls[0]?.body.has("device_label")).toBe(false);
  });

  it("surfaces the OAuth error code so the poll loop can read it", async () => {
    const { fetchImpl } = recordingFetch(
      jsonResponse(400, {
        error: "invalid_grant",
        error_description: "Another device already claimed this session.",
      }),
    );
    await expect(
      claimPairingSession(
        "https://gateway.example",
        {
          sessionCode: "mg_ps_abc",
          codeChallenge: "challenge",
          redirectUri: "tidebreak://callback",
        },
        fetchImpl,
      ),
    ).rejects.toMatchObject({
      name: "PairingError",
      code: "invalid_grant",
      status: 400,
    });
  });
});

describe("pollPairingSession", () => {
  it("sends the challenge and this app's client id with every poll", async () => {
    // The session code is printed in the QR and is public; the challenge is
    // what entitles this phone alone to collect the code.
    const { calls, fetchImpl } = recordingFetch(
      jsonResponse(200, { code: "mg_code_xyz" }),
    );
    const code = await pollPairingSession(
      "https://gateway.example/",
      "mg_ps_abc",
      "challenge",
      fetchImpl,
    );
    expect(code).toBe("mg_code_xyz");
    expect(Object.fromEntries(calls[0]?.body ?? [])).toEqual({
      session_code: "mg_ps_abc",
      code_challenge: "challenge",
      client_id: "tidebreak-mobile",
    });
  });

  it("rejects an approval that carries no code", async () => {
    const { fetchImpl } = recordingFetch(jsonResponse(200, {}));
    await expect(
      pollPairingSession("https://gateway.example", "mg_ps_abc", "c", fetchImpl),
    ).rejects.toThrow(/no authorization code/);
  });

  it("classifies a pending poll as pending rather than as a failure", async () => {
    const { fetchImpl } = recordingFetch(
      jsonResponse(400, { error: "authorization_pending" }),
    );
    const error = await pollPairingSession(
      "https://gateway.example",
      "mg_ps_abc",
      "c",
      fetchImpl,
    ).catch((caught: unknown) => caught);
    expect(isPairingPending(error)).toBe(true);
  });
});

describe("awaitPairingApproval", () => {
  const sleep = () => Promise.resolve();

  it("retries while the console has not decided, then returns the code", async () => {
    const poll = vi
      .fn<() => Promise<string>>()
      .mockRejectedValueOnce(
        new PairingError("pending", 400, "authorization_pending"),
      )
      .mockRejectedValueOnce(
        new PairingError("pending", 400, "authorization_pending"),
      )
      .mockResolvedValueOnce("mg_code_xyz");
    await expect(
      awaitPairingApproval({ poll, sleep, cancelled: () => false }),
    ).resolves.toEqual({ kind: "approved", code: "mg_code_xyz" });
    expect(poll).toHaveBeenCalledTimes(3);
  });

  it("stops without polling once cancelled", async () => {
    const poll = vi.fn<() => Promise<string>>();
    await expect(
      awaitPairingApproval({ poll, sleep, cancelled: () => true }),
    ).resolves.toEqual({ kind: "cancelled" });
    expect(poll).not.toHaveBeenCalled();
  });

  it("honours a cancel that lands while a poll is in flight", async () => {
    // The dangerous case: the user tapped Cancel, and the console approved
    // anyway. Signing them in from that would be signing them in behind their
    // back — the claim simply stays live server-side until denied or expired.
    let cancelled = false;
    const poll = vi.fn(async () => {
      cancelled = true;
      return "mg_code_xyz";
    });
    await expect(
      awaitPairingApproval({ poll, sleep, cancelled: () => cancelled }),
    ).resolves.toEqual({ kind: "cancelled" });
  });

  it("propagates a denial rather than looping on it", async () => {
    const poll = vi
      .fn<() => Promise<string>>()
      .mockRejectedValue(new PairingError("denied", 400, "access_denied"));
    await expect(
      awaitPairingApproval({ poll, sleep, cancelled: () => false }),
    ).rejects.toMatchObject({ code: "access_denied" });
    expect(poll).toHaveBeenCalledTimes(1);
  });
});

describe("pairingErrorMessage", () => {
  it.each([
    ["access_denied", /denied/i],
    ["expired_token", /no longer valid/i],
    ["invalid_grant", /no longer valid/i],
  ])("explains %s in the user's terms", (code, expected) => {
    expect(
      pairingErrorMessage(new PairingError("raw", 400, code)),
    ).toMatch(expected);
  });

  it("falls back to the gateway's own description", () => {
    expect(
      pairingErrorMessage(new PairingError("Too many attempts.", 429, "slow_down")),
    ).toBe("Too many attempts.");
  });
});
