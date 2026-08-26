import { describe, expect, it } from "vitest";
import { createPkcePair } from "./pkce";
import { sha256Base64Url } from "./crypto";

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
});
