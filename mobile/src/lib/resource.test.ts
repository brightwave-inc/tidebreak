import { describe, expect, it } from "vitest";
import {
  assertResourceEcho,
  isAllowedResource,
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

describe("isAllowedResource", () => {
  it("permits the union the merged app mints", () => {
    expect(isAllowedResource("control")).toBe(true);
    expect(isAllowedResource("control_plane")).toBe(true);
    expect(isAllowedResource("runtime:engineering")).toBe(true);
    expect(
      isAllowedResource(tidebreakMachineResource("https://machine.example.com")),
    ).toBe(true);
  });

  it("still refuses everything else", () => {
    // Inference and MCP credentials are not this client's business, whatever a
    // response asks it to mint.
    for (const resource of [
      "llm",
      "mcp:github",
      "connector",
      "sandbox:00000000-0000-0000-0000-000000000000",
      "control_planet",
      "",
    ]) {
      expect(isAllowedResource(resource)).toBe(false);
    }
  });
});
