import { describe, expect, it } from "vitest";
import {
  INITIAL_RECONNECT_DELAY_MS,
  MAX_RECONNECT_DELAY_MS,
  isCurrentConnection,
  nextReconnectDelay,
} from "./ChatEventStream";

describe("chat event reconnect helpers", () => {
  it("uses bounded exponential backoff", () => {
    expect(nextReconnectDelay(INITIAL_RECONNECT_DELAY_MS)).toBe(500);
    expect(nextReconnectDelay(4_000)).toBe(MAX_RECONNECT_DELAY_MS);
    expect(nextReconnectDelay(MAX_RECONNECT_DELAY_MS)).toBe(
      MAX_RECONNECT_DELAY_MS,
    );
  });

  it("rejects disposed and superseded socket callbacks", () => {
    expect(isCurrentConnection(false, 3, 3)).toBe(true);
    expect(isCurrentConnection(true, 3, 3)).toBe(false);
    expect(isCurrentConnection(false, 3, 4)).toBe(false);
  });
});
