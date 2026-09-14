import { describe, expect, it } from "vitest";
import {
  BASELINE_SCOPE,
  grantedScopeFrom,
  grantsConsoleRead,
  grantsConsoleWrite,
  grantsRuntimeExecute,
  requestedScope,
} from "./scope";
import type { GatewayMeta } from "./types";

function meta(surfaces?: GatewayMeta["surfaces"]): GatewayMeta {
  return { installation_id: "inst-1", ...(surfaces ? { surfaces } : {}) };
}

describe("requestedScope", () => {
  it("asks a gateway that advertises nothing for the baseline only", () => {
    // Every gateway deployed today. Asking for a scope it does not know
    // breaks sign-in outright rather than narrowing the session.
    expect(requestedScope(meta())).toBe(BASELINE_SCOPE);
    expect(requestedScope(null)).toBe(BASELINE_SCOPE);
    expect(requestedScope(undefined)).toBe(BASELINE_SCOPE);
  });

  it("does not read the binary-wide write advertisement as console access", () => {
    // surfaces.control_plane_write is true on gateways whose tidebreak-mobile
    // client is still confined to control + tidebreak:*. Treating it as
    // permission to request control-plane scopes would break sign-in against
    // all of them.
    expect(requestedScope(meta({ control_plane_write: true }))).toBe(
      BASELINE_SCOPE,
    );
  });

  it("asks for the console scopes once this client is widened", () => {
    expect(requestedScope(meta({ tidebreak_mobile_console: true }))).toBe(
      "openid profile offline_access control_plane:read runtime:execute",
    );
  });

  it("adds the write scope only when the gateway accepts it", () => {
    expect(
      requestedScope(
        meta({ tidebreak_mobile_console: true, control_plane_write: true }),
      ),
    ).toBe(
      "openid profile offline_access control_plane:read runtime:execute control_plane:write",
    );
  });
});

describe("grantedScopeFrom", () => {
  it("records what the gateway said it granted", () => {
    expect(
      grantedScopeFrom({
        scope: "openid profile offline_access control_plane:read",
      }),
    ).toBe("openid profile offline_access control_plane:read");
  });

  it("records nothing when the response omits the scope", () => {
    // RFC 6749 lets a server omit `scope` to mean "as requested". Recording
    // the request on that reading would hand the UI console authority the
    // gateway never named, so the omission stays unknown — and unknown is
    // baseline.
    expect(grantedScopeFrom({})).toBeUndefined();
    expect(grantedScopeFrom({ scope: "" })).toBeUndefined();
    expect(grantedScopeFrom({ scope: "   " })).toBeUndefined();
  });

  it("gates the console off for a session whose grant was never returned", () => {
    const granted = grantedScopeFrom({});
    expect(grantsConsoleRead(granted)).toBe(false);
    expect(grantsRuntimeExecute(granted)).toBe(false);
    expect(grantsConsoleWrite(granted)).toBe(false);
  });
});

describe("the recorded grant", () => {
  it("answers about the session that signed in, not about the client", () => {
    const granted = "openid profile offline_access control_plane:read";
    expect(grantsConsoleRead(granted)).toBe(true);
    expect(grantsConsoleWrite(granted)).toBe(false);
    expect(grantsRuntimeExecute(granted)).toBe(false);
  });

  it("reads an unrecorded grant as no authority", () => {
    expect(grantsConsoleRead(undefined)).toBe(false);
    expect(grantsConsoleWrite("")).toBe(false);
  });

  it("does not match a scope by prefix", () => {
    expect(grantsConsoleWrite("control_plane:read")).toBe(false);
    expect(grantsConsoleRead("control_plane:read_only")).toBe(false);
  });
});
