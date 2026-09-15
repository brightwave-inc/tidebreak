import { describe, expect, it } from "vitest";
import {
  MACHINE_TOKEN_MAX_LENGTH,
  MACHINE_TOKEN_MIN_LENGTH,
  formatMachineQr,
  isMachineToken,
  machineTokenProblem,
  parseMachineQr,
} from "./machineToken";

const TOKEN = "a".repeat(64);

describe("isMachineToken", () => {
  it("accepts the roster's alphabet at its floor", () => {
    expect(isMachineToken("a".repeat(MACHINE_TOKEN_MIN_LENGTH))).toBe(true);
    // `auth.rs` allows exactly these four punctuation bytes, because the same
    // string has to survive a header and a WebSocket subprotocol.
    expect(isMachineToken(`${"a".repeat(28)}._~-`)).toBe(true);
  });

  it("refuses what the machine's own loader would refuse", () => {
    expect(isMachineToken("a".repeat(MACHINE_TOKEN_MIN_LENGTH - 1))).toBe(false);
    expect(isMachineToken("a".repeat(MACHINE_TOKEN_MAX_LENGTH + 1))).toBe(false);
    // A space would break the header; a colon and a slash would let a URL
    // masquerade as a token.
    expect(isMachineToken(`${"a".repeat(32)} b`)).toBe(false);
    expect(isMachineToken(`${"a".repeat(32)}/x`)).toBe(false);
    expect(isMachineToken(`${"a".repeat(32)}\n`)).toBe(false);
  });
});

describe("machineTokenProblem", () => {
  it("names each rejection distinctly, and passes a usable token", () => {
    expect(machineTokenProblem("   ")).toBe("empty");
    expect(machineTokenProblem("short")).toBe("too_short");
    expect(machineTokenProblem("a".repeat(600))).toBe("too_long");
    expect(machineTokenProblem(`${"a".repeat(32)} oops`)).toBe("charset");
    expect(machineTokenProblem(`  ${TOKEN}  `)).toBeNull();
  });
});

describe("parseMachineQr", () => {
  it("round-trips a well-formed payload", () => {
    const payload = { baseUrl: "https://machine.example.test", token: TOKEN };
    expect(parseMachineQr(formatMachineQr(payload))).toEqual(payload);
  });

  it("normalizes the URL through the same rule attach uses", () => {
    expect(
      parseMachineQr(
        `tidebreak-machine://v1?url=${encodeURIComponent("https://machine.example.test/")}&token=${TOKEN}`,
      ),
    ).toEqual({ baseUrl: "https://machine.example.test", token: TOKEN });
  });

  it("refuses a plaintext address to anything but the phone itself", () => {
    expect(
      parseMachineQr(
        `tidebreak-machine://v1?url=${encodeURIComponent("http://machine.example.test")}&token=${TOKEN}`,
      ),
    ).toBeNull();
    // Loopback is the development case the URL rule allows, and the payload
    // must not be stricter than the attach it feeds.
    expect(
      parseMachineQr(
        `tidebreak-machine://v1?url=${encodeURIComponent("http://127.0.0.1:8080")}&token=${TOKEN}`,
      ),
    ).toEqual({ baseUrl: "http://127.0.0.1:8080", token: TOKEN });
  });

  it("refuses a payload whose token could not be a roster token", () => {
    expect(
      parseMachineQr(
        `tidebreak-machine://v1?url=${encodeURIComponent("https://machine.example.test")}&token=short`,
      ),
    ).toBeNull();
    expect(
      parseMachineQr(
        `tidebreak-machine://v1?url=${encodeURIComponent("https://machine.example.test")}`,
      ),
    ).toBeNull();
  });

  it("refuses every other scheme, including this app's own deep links", () => {
    // The point of an unregistered scheme: a `tidebreak://` link a web page
    // can open must never be able to hand this phone a machine and a token.
    expect(
      parseMachineQr(
        `tidebreak://attach?url=${encodeURIComponent("https://machine.example.test")}&token=${TOKEN}`,
      ),
    ).toBeNull();
    expect(
      parseMachineQr(
        `https://machine.example.test/?token=${TOKEN}`,
      ),
    ).toBeNull();
    expect(parseMachineQr("not a url at all")).toBeNull();
    expect(parseMachineQr("")).toBeNull();
  });

  it("refuses an unknown payload version rather than guessing", () => {
    expect(
      parseMachineQr(
        `tidebreak-machine://v2?url=${encodeURIComponent("https://machine.example.test")}&token=${TOKEN}`,
      ),
    ).toBeNull();
  });

  it("refuses an oversized payload without parsing it", () => {
    expect(parseMachineQr(`tidebreak-machine://v1?x=${"a".repeat(4096)}`)).toBeNull();
  });
});
