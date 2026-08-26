import { describe, expect, it } from "vitest";
import {
  assertResourceEcho,
  tidebreakMachineResource,
} from "./resource";
import { validatedBaseUrl } from "./url";

describe("tidebreakMachineResource", () => {
  it("matches the known desktop vector", () => {
    expect(tidebreakMachineResource("https://tidebreak.example.test")).toBe(
      "tidebreak:3c6444cbec9b33f56b4ed0f1bf7015741c69cf7e516977c52975c6a0012a097b",
    );
  });

  it("is stable for the same canonical URL and distinct across hosts", () => {
    const canonical = validatedBaseUrl("https://machine.example.com/");
    expect(tidebreakMachineResource(canonical)).toBe(
      tidebreakMachineResource(
        validatedBaseUrl(" https://machine.example.com "),
      ),
    );
    expect(tidebreakMachineResource(canonical)).not.toBe(
      tidebreakMachineResource("https://other.example.test"),
    );
  });

  it("rejects an echoed resource that does not match the derived value", () => {
    const derived = tidebreakMachineResource("https://machine.example.com");
    expect(() =>
      assertResourceEcho(derived, "tidebreak:deadbeef"),
    ).toThrow(/does not match/);
  });
});
