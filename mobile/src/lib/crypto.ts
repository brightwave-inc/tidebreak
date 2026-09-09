import { getRandomBytes } from "expo-crypto";
import { sha256 } from "js-sha256";

// Hermes provides no `crypto` or `btoa` globals in a release build (dev
// tooling polyfills them, so the gap only surfaces on device). Randomness
// comes from expo-crypto and base64url is encoded by hand.
const BASE64URL_ALPHABET =
  "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

function base64UrlEncode(bytes: ArrayLike<number>): string {
  let out = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i] ?? 0;
    const b1 = i + 1 < bytes.length ? bytes[i + 1] : undefined;
    const b2 = i + 2 < bytes.length ? bytes[i + 2] : undefined;
    out += BASE64URL_ALPHABET[b0 >> 2];
    out += BASE64URL_ALPHABET[((b0 & 0x03) << 4) | ((b1 ?? 0) >> 4)];
    if (b1 === undefined) break;
    out += BASE64URL_ALPHABET[((b1 & 0x0f) << 2) | ((b2 ?? 0) >> 6)];
    if (b2 === undefined) break;
    out += BASE64URL_ALPHABET[b2 & 0x3f];
  }
  return out;
}

export function sha256Hex(input: string): string {
  return sha256(input);
}

export function sha256Base64Url(input: string): string {
  return base64UrlEncode(sha256.array(input));
}

export function randomUrlSafe(byteLength = 32): string {
  return base64UrlEncode(getRandomBytes(byteLength));
}
