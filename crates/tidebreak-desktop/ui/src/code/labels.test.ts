import { describe, expect, it } from "vitest";

import type { ModeCaps } from "./labels";
import {
  autoIsUnsupervised,
  createPermissionModes,
  defaultCreatePermissionMode,
  harnessUnusableReason,
} from "./labels";

function caps(
  plan_mode: ModeCaps["plan_mode"],
  structured_approvals: ModeCaps["structured_approvals"],
  auto_mode: ModeCaps["auto_mode"],
  allow_mode: ModeCaps["allow_mode"] = "unsupported",
): ModeCaps {
  return { plan_mode, structured_approvals, auto_mode, allow_mode };
}

describe("create-time permission mode", () => {
  it("defaults to Ask when the doctor reports structured approvals", () => {
    expect(defaultCreatePermissionMode(caps("supported", "supported", "supported"))).toBe(
      "ask",
    );
    expect(
      createPermissionModes(caps("supported", "supported", "supported", "supported")),
    ).toEqual(["plan", "ask", "auto", "allow"]);
  });

  it("falls back to Plan when structured approvals are not supported", () => {
    expect(
      defaultCreatePermissionMode(caps("supported", "unsupported", "unsupported")),
    ).toBe("plan");
    expect(
      defaultCreatePermissionMode(caps("supported", "unknown", "unknown")),
    ).toBe("plan");
    expect(
      createPermissionModes(caps("supported", "unsupported", "unsupported")),
    ).toEqual(["plan"]);
  });

  it("offers only unsupervised Auto for a grok-shaped engine", () => {
    const grok = caps("unsupported", "unsupported", "supported");
    expect(createPermissionModes(grok)).toEqual(["auto"]);
    expect(createPermissionModes(caps("unsupported", "unsupported", "supported", "supported"))).toEqual(
      ["auto", "allow"],
    );
    expect(defaultCreatePermissionMode(grok)).toBe("auto");
    expect(autoIsUnsupervised(grok)).toBe(true);
    // Supervised Auto rides the approval channel and needs no statement.
    expect(autoIsUnsupervised(caps("supported", "supported", "supported"))).toBe(
      false,
    );
  });
});

describe("harnessUnusableReason", () => {
  it("names the one reason a picker row cannot be chosen", () => {
    expect(
      harnessUnusableReason({
        found: false,
        caps: caps("supported", "supported", "supported"),
      }),
    ).toBe("Not installed");
    expect(
      harnessUnusableReason({
        found: true,
        authenticated: false,
        caps: caps("supported", "supported", "supported"),
      }),
    ).toBe("Sign in via your terminal");
    expect(
      harnessUnusableReason({
        found: true,
        caps: caps("unsupported", "unsupported", "unsupported"),
      }),
    ).toBe("Not available yet");
    expect(
      harnessUnusableReason({
        found: true,
        caps: caps("supported", "unsupported", "unsupported"),
      }),
    ).toBeNull();
    // An Auto-only engine is usable, not "Not available yet".
    expect(
      harnessUnusableReason({
        found: true,
        caps: caps("unsupported", "unsupported", "supported"),
      }),
    ).toBeNull();
  });
});
