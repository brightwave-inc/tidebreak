import { describe, expect, it } from "vitest";

import {
  createPermissionModes,
  defaultCreatePermissionMode,
  harnessUnusableReason,
} from "./labels";

describe("create-time permission mode", () => {
  it("defaults to Ask when the doctor reports structured approvals", () => {
    expect(defaultCreatePermissionMode("supported")).toBe("ask");
    expect(createPermissionModes("supported")).toEqual(["plan", "ask", "auto"]);
  });

  it("falls back to Plan when structured approvals are not supported", () => {
    expect(defaultCreatePermissionMode("unsupported")).toBe("plan");
    expect(defaultCreatePermissionMode("unknown")).toBe("plan");
    expect(defaultCreatePermissionMode(undefined)).toBe("plan");
    expect(createPermissionModes("unsupported")).toEqual(["plan"]);
    expect(createPermissionModes("unknown")).toEqual(["plan"]);
  });
});

describe("harnessUnusableReason", () => {
  it("names the one reason a picker row cannot be chosen", () => {
    expect(
      harnessUnusableReason({
        found: false,
        caps: { plan_mode: "supported", structured_approvals: "supported" },
      }),
    ).toBe("Not installed");
    expect(
      harnessUnusableReason({
        found: true,
        authenticated: false,
        caps: { plan_mode: "supported", structured_approvals: "supported" },
      }),
    ).toBe("Sign in via your terminal");
    expect(
      harnessUnusableReason({
        found: true,
        caps: { plan_mode: "unsupported", structured_approvals: "unsupported" },
      }),
    ).toBe("Not available yet");
    expect(
      harnessUnusableReason({
        found: true,
        caps: { plan_mode: "supported", structured_approvals: "unsupported" },
      }),
    ).toBeNull();
  });
});
