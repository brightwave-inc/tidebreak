import { describe, expect, it } from "vitest";

import { hostAuthorityRefusal } from "./host";

/**
 * The four codes are the contract with the native shell. They are written out
 * here rather than imported, so a rename on either side fails this test instead
 * of passing silently.
 */
describe("host authority refusals", () => {
  it("names the authority a remote-attached client lost", () => {
    expect(hostAuthorityRefusal("folder_broker_authority_unavailable")).toBe("folder_broker");
    expect(hostAuthorityRefusal("client_executor_authority_unavailable")).toBe("client_executor");
    expect(hostAuthorityRefusal("native_export_authority_unavailable")).toBe("native_export");
    expect(hostAuthorityRefusal("computer_use_authority_unavailable")).toBe("computer_use");
  });

  it("leaves every other failure alone", () => {
    expect(hostAuthorityRefusal("host broker returned an unexpected response")).toBeNull();
    expect(hostAuthorityRefusal("folder_broker_authority_unavailable extra")).toBeNull();
    expect(hostAuthorityRefusal(new Error("folder_broker_authority_unavailable"))).toBeNull();
    expect(hostAuthorityRefusal(undefined)).toBeNull();
  });
});
