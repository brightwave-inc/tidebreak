import { describe, expect, it } from "vitest";
import { isPairingSessionHandle, parsePairingScan } from "./provision";

describe("parsePairingScan", () => {
  it("reads a provision link from this app's own scheme", () => {
    expect(
      parsePairingScan(
        "tidebreak://provision?gateway=https%3A%2F%2Fgateway.example&session=mg_ps_abc123",
      ),
    ).toEqual({
      gatewayUrl: "https://gateway.example",
      sessionCode: "mg_ps_abc123",
    });
  });

  it.each([
    "tidebreak-staging",
    "tidebreak-dev",
    // The gateway console's pairing page still encodes its QR with the
    // Tidewatch schemes (TidewatchPage.tsx predates the merged app). The
    // session behind the link is client-neutral, so the envelope scheme is
    // accepted; only the in-app scanner ever reads it.
    "mg-tidewatch",
    "mg-tidewatch-staging",
    "mg-tidewatch-dev",
  ])(
    "accepts a %s build's scheme too",
    (scheme) => {
      // Deliberate: a staging phone must be able to scan a production
      // console's code. The payload is inert either way.
      expect(
        parsePairingScan(
          `${scheme}://provision?gateway=https%3A%2F%2Fgateway.example&session=mg_ps_abc`,
        ),
      ).toEqual({
        gatewayUrl: "https://gateway.example",
        sessionCode: "mg_ps_abc",
      });
    },
  );

  it.each([
    "tidebreak-other://provision?gateway=https%3A%2F%2Fgateway.example",
    "mg-tidewatch-other://provision?gateway=https%3A%2F%2Fgateway.example",
    "javascript://provision?gateway=https%3A%2F%2Fgateway.example",
  ])("refuses the unallowlisted scheme in %j", (payload) => {
    expect(parsePairingScan(payload)).toBeNull();
  });

  it("refuses a link on our scheme that is not a provision link", () => {
    // The OAuth callback shares the scheme; only the host separates them, and
    // treating a callback as a pairing scan would send an authorization code
    // into the claim form.
    expect(
      parsePairingScan("tidebreak://callback?code=abc&state=xyz"),
    ).toBeNull();
  });

  it("drops a session handle that is not shaped like one", () => {
    // Still a usable scan — the gateway is real — but nothing gets smuggled
    // into the claim form, and sign-in falls back to the browser.
    expect(
      parsePairingScan(
        "tidebreak://provision?gateway=https%3A%2F%2Fgateway.example&session=../../evil",
      ),
    ).toEqual({ gatewayUrl: "https://gateway.example", sessionCode: null });
  });

  it("refuses a provision link whose gateway is not a safe base URL", () => {
    expect(
      parsePairingScan(
        "tidebreak://provision?gateway=http%3A%2F%2Fgateway.example&session=mg_ps_abc",
      ),
    ).toBeNull();
    expect(
      parsePairingScan("tidebreak://provision?session=mg_ps_abc"),
    ).toBeNull();
    expect(
      parsePairingScan(
        "tidebreak://provision?gateway=https%3A%2F%2Fuser%3Apass%40gateway.example",
      ),
    ).toBeNull();
  });

  it("accepts a bare gateway URL, which pairs through the browser", () => {
    expect(parsePairingScan("  https://gateway.example/  ")).toEqual({
      gatewayUrl: "https://gateway.example",
      sessionCode: null,
    });
  });

  it("allows http only for a loopback host, as the URL rules do", () => {
    expect(parsePairingScan("http://localhost:8080")).toEqual({
      gatewayUrl: "http://localhost:8080",
      sessionCode: null,
    });
    expect(parsePairingScan("http://gateway.example")).toBeNull();
  });

  it.each(["", "   ", "not a url", "a".repeat(4096)])(
    "returns null for junk (%j)",
    (payload) => {
      expect(parsePairingScan(payload)).toBeNull();
    },
  );
});

describe("isPairingSessionHandle", () => {
  it.each(["mg_ps_abc123", "mg_ps_A-b_C"])("accepts %s", (value) => {
    expect(isPairingSessionHandle(value)).toBe(true);
  });

  it.each(["mg_ps_", "mg_code_abc", "abc", "mg_ps_abc/def", "mg_ps_ab c"])(
    "refuses %j",
    (value) => {
      expect(isPairingSessionHandle(value)).toBe(false);
    },
  );
});
