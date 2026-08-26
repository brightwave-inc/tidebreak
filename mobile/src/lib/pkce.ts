import { randomUrlSafe, sha256Base64Url } from "./crypto";

export type PkcePair = {
  verifier: string;
  challenge: string;
  method: "S256";
};

export function createPkcePair(verifier = randomUrlSafe(32)): PkcePair {
  if (verifier.length < 43 || verifier.length > 128) {
    throw new Error("PKCE verifier must be 43–128 characters");
  }
  return {
    verifier,
    challenge: sha256Base64Url(verifier),
    method: "S256",
  };
}
