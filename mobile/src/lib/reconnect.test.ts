import { describe, expect, it } from "vitest";
import {
  INITIAL_RECONNECT_DELAY_MS,
  MAX_RECONNECT_DELAY_MS,
  jitteredDelay,
  nextReconnectDelay,
} from "./reconnect";

describe("nextReconnectDelay", () => {
  it("doubles until the cap", () => {
    expect(nextReconnectDelay(INITIAL_RECONNECT_DELAY_MS)).toBe(500);
    expect(nextReconnectDelay(4_000)).toBe(MAX_RECONNECT_DELAY_MS);
    expect(nextReconnectDelay(MAX_RECONNECT_DELAY_MS)).toBe(
      MAX_RECONNECT_DELAY_MS,
    );
  });
});

describe("jitteredDelay", () => {
  it("stays within 50–150% of the step", () => {
    expect(jitteredDelay(1000, () => 0)).toBe(500);
    expect(jitteredDelay(1000, () => 1)).toBe(1500);
    expect(jitteredDelay(1000, () => 0.5)).toBe(1000);
  });
});
