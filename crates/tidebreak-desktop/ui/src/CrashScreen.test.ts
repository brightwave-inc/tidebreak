import { describe, expect, it } from "vitest";

import { crashDebugReport, reportRoutePath } from "./CrashScreen";

/**
 * Credential-shaped values, assembled at run time so no scanner mistakes this
 * file for a leak.
 */
function fakeCredentials() {
  const githubToken = ["ghs", "_", "A1b2C3d4".repeat(4)].join("");
  const bearer = ["tidebreak", "-token.", "q7Rk2Lm9".repeat(4)].join("");
  const queryToken = ["z", "9y8x7w6v".repeat(3)].join("");
  const nonce = ["n", "4f2d9c1b".repeat(3)].join("");
  const cloneUrl = [
    "https://",
    "x-access-token",
    ":",
    githubToken,
    "@github.com/acme/app.git?",
    "token=",
    queryToken,
  ].join("");
  return { githubToken, bearer, queryToken, nonce, cloneUrl };
}

function report(overrides: Partial<Parameters<typeof crashDebugReport>[0]>) {
  return crashDebugReport({
    error: new Error("boom"),
    route: "/",
    appVersion: "0.120.0",
    capturedAt: "2026-09-24T12:00:00.000Z",
    userAgent: null,
    ...overrides,
  });
}

describe("crashDebugReport", () => {
  it("keeps credentials out of the message, the stacks, and the route", () => {
    const { githubToken, bearer, queryToken, nonce, cloneUrl } =
      fakeCredentials();
    const error = new Error(
      `clone failed for ${cloneUrl}; Authorization: Bearer ${bearer}`,
    );
    error.stack = `Error: clone failed for ${cloneUrl}\n    at clone (app.js:1:1)`;

    const copied = report({
      error,
      componentStack: `\n    at CloneStatus (${cloneUrl})`,
      route: `/connect/${nonce}`,
    });

    for (const secret of [githubToken, bearer, queryToken, nonce]) {
      expect(copied).not.toContain(secret);
    }
    const parsed = JSON.parse(copied);
    expect(parsed.route).toBe("/connect/[redacted]");
    // The report still says what failed and where.
    expect(parsed.error.message).toContain("clone failed for https://");
    expect(parsed.error.stack).toContain("at clone (app.js:1:1)");
  });

  it("keeps an ordinary route as it is", () => {
    expect(JSON.parse(report({ route: "/code/w/workspace-1" })).route).toBe(
      "/code/w/workspace-1",
    );
  });
});

describe("reportRoutePath", () => {
  it("redacts the parameter of a route that carries a capability", () => {
    const { nonce } = fakeCredentials();
    expect(reportRoutePath(`/connect/${nonce}`)).toBe("/connect/[redacted]");
    expect(reportRoutePath(`/workspace-grant/${nonce}`)).toBe(
      "/workspace-grant/[redacted]",
    );
    expect(reportRoutePath("/settings/providers")).toBe("/settings/providers");
  });
});
