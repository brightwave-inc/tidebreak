import { describe, expect, it } from "vitest";

import {
  formatElapsedDuration,
  formatTurnDuration,
  isCodexRevokedRefreshTokenError,
} from "./TurnReviewCard";

describe("isCodexRevokedRefreshTokenError", () => {
  it("recognizes the Codex CLI diagnostic through prefixes and line wrapping", () => {
    expect(
      isCodexRevokedRefreshTokenError(
        "Error: Your access token could not be refreshed because your refresh\n token was revoked. Please log out and sign in again.",
      ),
    ).toBe(true);
  });

  it("leaves other engine and authentication failures unchanged", () => {
    expect(isCodexRevokedRefreshTokenError(null)).toBe(false);
    expect(isCodexRevokedRefreshTokenError("claude exited with status 1")).toBe(
      false,
    );
    expect(
      isCodexRevokedRefreshTokenError(
        "Your access token expired. Please sign in again.",
      ),
    ).toBe(false);
  });
});

describe("formatTurnDuration", () => {
  it("never rounds a turn that happened to zero", () => {
    expect(formatTurnDuration(0)).toBe("<1s");
    expect(formatTurnDuration(420)).toBe("<1s");
    expect(formatTurnDuration(999)).toBe("<1s");
  });

  it("carries a tenth while a tenth still separates two turns", () => {
    expect(formatTurnDuration(1_000)).toBe("1.0s");
    expect(formatTurnDuration(2_400)).toBe("2.4s");
    expect(formatTurnDuration(9_949)).toBe("9.9s");
  });

  it("drops to whole seconds, then minutes, then hours", () => {
    // The tenth rounds up to ten before the millisecond count does, and the
    // seam must not read "10.0s" one tick and "10s" the next.
    expect(formatTurnDuration(9_999)).toBe("10s");
    expect(formatTurnDuration(10_000)).toBe("10s");
    expect(formatTurnDuration(59_400)).toBe("59s");
    expect(formatTurnDuration(72_000)).toBe("1m 12s");
    expect(formatTurnDuration(134_000)).toBe("2m 14s");
    expect(formatTurnDuration(3_930_000)).toBe("1h 5m");
  });

  it("reports nothing rather than a made-up figure", () => {
    expect(formatTurnDuration(null)).toBeNull();
    expect(formatTurnDuration(-1)).toBeNull();
    expect(formatTurnDuration(Number.NaN)).toBeNull();
  });
});

describe("formatElapsedDuration", () => {
  it("stays on whole seconds, because it is read while it ticks", () => {
    expect(formatElapsedDuration(300)).toBe("<1s");
    expect(formatElapsedDuration(2_400)).toBe("2s");
    expect(formatElapsedDuration(72_000)).toBe("1m 12s");
  });
});
