import { afterEach, describe, expect, it, vi } from "vitest";
import { computerUsePermissionHost } from "./computerUsePermissions";
const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  native: true,
  remote: false,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("./host", () => ({
  hasNativeHost: () => mocks.native,
  attachedRemotely: () => mocks.remote,
}));
afterEach(() => {
  vi.clearAllMocks();
  mocks.native = true;
  mocks.remote = false;
});
describe("computerUsePermissionHost", () => {
  it("uses separate native commands for pure status and explicit permission requests", async () => {
    await computerUsePermissionHost.status();
    expect(mocks.invoke).toHaveBeenCalledWith("computer_use_permission_status");
    expect(mocks.invoke).not.toHaveBeenCalledWith(
      "request_computer_use_permissions",
    );
    await computerUsePermissionHost.request();
    expect(mocks.invoke).toHaveBeenLastCalledWith(
      "request_computer_use_permissions",
    );
    await computerUsePermissionHost.openSettings("screen_recording");
    expect(mocks.invoke).toHaveBeenLastCalledWith(
      "open_computer_use_permission_settings",
      { pane: "screen_recording" },
    );
  });
  it.each(["remote", "web"])(
    "refuses %s use before invoking the local helper",
    (mode) => {
      mocks.native = mode !== "web";
      mocks.remote = mode === "remote";
      expect(computerUsePermissionHost.availability()).toBe(mode);
      expect(() => computerUsePermissionHost.status()).toThrow();
      expect(() => computerUsePermissionHost.request()).toThrow();
      expect(() =>
        computerUsePermissionHost.openSettings("accessibility"),
      ).toThrow();
      expect(mocks.invoke).not.toHaveBeenCalled();
    },
  );
});
