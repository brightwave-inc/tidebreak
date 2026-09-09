import { describe, expect, it } from "vitest";
import { createPkcePair } from "./pkce";
import { randomUrlSafe, sha256Base64Url } from "./crypto";

describe("PKCE S256", () => {
  it("hashes the verifier with S256 / base64url", () => {
    const verifier = "a".repeat(43);
    const pair = createPkcePair(verifier);
    expect(pair.method).toBe("S256");
    expect(pair.verifier).toBe(verifier);
    expect(pair.challenge).toBe(sha256Base64Url(verifier));
    expect(pair.challenge).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(pair.challenge.includes("=")).toBe(false);
  });

  it("matches the RFC 7636 appendix B vector", () => {
    // Pins the hand-rolled base64url encoder (Hermes has no btoa) to the
    // spec's known answer; a wrong encoding breaks every token exchange.
    expect(sha256Base64Url("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")).toBe(
      "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
    );
  });

  it("generates url-safe verifiers of the expected length", () => {
    const value = randomUrlSafe(32);
    expect(value).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(randomUrlSafe(32)).not.toBe(value);
  });
});
