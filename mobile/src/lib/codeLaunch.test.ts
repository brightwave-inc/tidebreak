import { describe, expect, it } from "vitest";
import type { CodeHarnessOption } from "./api";
import {
  createPermissionModes,
  defaultCreatePermissionMode,
  harnessUnavailableReason,
  permissionModeWarning,
  permittedPermissionModes,
} from "./codeLaunch";

const harness: CodeHarnessOption = {
  kind: "codex",
  found: true,
  installable: true,
  authenticated: true,
  auth_mode: "local_sign_in",
  remediation: "",
  caps: {
    resume: "supported",
    streaming_deltas: "supported",
    structured_approvals: "supported",
    mid_turn_steering: "supported",
    plan_mode: "supported",
    auto_mode: "supported",
    allow_mode: "supported",
    reasoning_levels: "supported",
    native_file_change_events: "supported",
    native_interrupt: "supported",
    image_input: "unsupported",
    slash_commands: "supported",
  },
};

describe("mobile code launch choices", () => {
  it("offers only modes the harness supports and policy permits", () => {
    expect(createPermissionModes(harness.caps)).toEqual([
      "plan",
      "ask",
      "auto",
      "allow",
    ]);
    const capped = permittedPermissionModes(harness.caps, "ask");
    expect(capped).toEqual(["plan", "ask"]);
    expect(defaultCreatePermissionMode(capped)).toBe("ask");
  });

  it("states why a harness cannot start instead of leaving a dead control", () => {
    expect(harnessUnavailableReason(harness)).toBeNull();
    expect(
      harnessUnavailableReason({
        ...harness,
        found: false,
        authenticated: undefined,
      }),
    ).toMatch(/desktop first/);
    expect(
      harnessUnavailableReason({
        ...harness,
        auth_mode: "hosted_unavailable",
      }),
    ).toMatch(/hosted machines/);
    expect(
      harnessUnavailableReason({
        ...harness,
        authenticated: false,
        remediation: "Run codex login.",
      }),
    ).toBe("Run codex login.");
  });

  it("warns when the selected posture has nobody to ask", () => {
    expect(permissionModeWarning("allow", harness.caps)).toMatch(
      /without asking/,
    );
    expect(permissionModeWarning("auto", harness.caps)).toBeNull();
    expect(
      permissionModeWarning("auto", {
        ...harness.caps,
        structured_approvals: "unsupported",
      }),
    ).toMatch(/no approval channel/);
  });
});

