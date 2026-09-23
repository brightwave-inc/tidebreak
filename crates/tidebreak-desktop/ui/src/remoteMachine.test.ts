import { describe, expect, it } from "vitest";

import {
  HOST_AUTHORITIES,
  connectFailureMessage,
  hostAuthorityLabel,
  hostErrorMessage,
  remoteConnectError,
} from "./remoteMachine";

describe("remote connect refusals", () => {
  /**
   * The shell sends a reason and nothing else. Every reason it can send needs
   * copy here, or a user meets a blank where an explanation belongs.
   */
  it("words every reason the shell can send", () => {
    const reasons = [
      "remote_machine_url_invalid",
      "remote_machine_requires_tls",
      "remote_machine_unreachable",
      "remote_machine_token_refused",
      "remote_machine_not_a_machine",
      "remote_machine_token_storage_failed",
      "remote_machine_gateway_auth_unavailable",
      "remote_machine_newer_than_app",
      "remote_machine_older_than_app",
    ] as const;
    for (const reason of reasons) {
      const refused = remoteConnectError({ reason, detail: null });
      expect(refused, reason).not.toBeNull();
      expect(connectFailureMessage(refused!).length).toBeGreaterThan(0);
    }
  });

  it("says why https is required, because that is the refusal a reader can act on", () => {
    const refused = remoteConnectError({
      reason: "remote_machine_requires_tls",
      detail: null,
    });
    expect(connectFailureMessage(refused!)).toContain("https");
  });

  it("names the machine's release when the versions do not match", () => {
    const newer = remoteConnectError({
      reason: "remote_machine_newer_than_app",
      detail: null,
      machineVersion: "9.4.0",
    });
    expect(connectFailureMessage(newer!)).toBe(
      "That machine runs Tidebreak 9.4.0. Update Tidebreak to 9.4.0 or later to connect.",
    );
    const older = remoteConnectError({
      reason: "remote_machine_older_than_app",
      detail: null,
      machineVersion: "0.9.0",
    });
    expect(connectFailureMessage(older!)).toContain("Update the machine");
    expect(connectFailureMessage(older!)).toContain("0.9.0");
    // A refusal without the release still says which side to update.
    const unnamed = remoteConnectError({
      reason: "remote_machine_newer_than_app",
      detail: null,
    });
    expect(connectFailureMessage(unnamed!)).toContain("Update Tidebreak");
  });

  it("leaves anything that is not a worded refusal to the ordinary failure path", () => {
    expect(
      remoteConnectError({ reason: "remote_machine_invented_reason" }),
    ).toBeNull();
    expect(remoteConnectError("remote_machine_requires_tls")).toBeNull();
    expect(remoteConnectError(null)).toBeNull();
    // An inherited property name is not a reason.
    expect(remoteConnectError({ reason: "toString" })).toBeNull();
  });
});

describe("host authority refusals reaching a reader", () => {
  it("names the capability and the reason instead of the raw code", () => {
    const message = hostErrorMessage(
      "folder_broker_authority_unavailable",
      "fallback",
    );
    expect(message).toContain(hostAuthorityLabel("folder_broker"));
    expect(message).toContain("remote machine");
    expect(message).not.toContain("folder_broker_authority_unavailable");
  });

  it("covers all four authorities", () => {
    for (const authority of HOST_AUTHORITIES) {
      expect(hostAuthorityLabel(authority).length).toBeGreaterThan(0);
    }
    expect(HOST_AUTHORITIES).toHaveLength(4);
  });

  it("falls through for an ordinary failure", () => {
    expect(
      hostErrorMessage(
        "host broker returned an unexpected response",
        "fallback",
      ),
    ).toBe("host broker returned an unexpected response");
  });
});
