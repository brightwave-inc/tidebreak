import { describe, expect, it } from "vitest";

import {
  createPermissionModes,
  defaultCreatePermissionMode,
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
