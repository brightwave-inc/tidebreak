import { invoke } from "@tauri-apps/api/core";
import { attachedRemotely, hasNativeHost } from "./host";

export type ComputerUsePermissionPane = "accessibility" | "screen_recording";
export type ComputerUsePermissionStatus =
  | {
      status: "available";
      appName: string;
      appIdentifier: string | null;
      screenRecording: boolean;
      accessibility: boolean;
    }
  | { status: "unsupported" };

export interface ComputerUsePermissionHost {
  availability(): "local" | "remote" | "web";
  status(): Promise<ComputerUsePermissionStatus>;
  request(): Promise<ComputerUsePermissionStatus>;
  openSettings(pane: ComputerUsePermissionPane): Promise<void>;
  /**
   * Quit and reopen Tidebreak. macOS applies a Screen Recording grant only to
   * a process started after it. Working agents get the quit prompt first.
   */
  restart(): Promise<void>;
}

function requireLocalHost(): void {
  if (!hasNativeHost() || attachedRemotely()) {
    throw new Error("Computer-use setup requires Tidebreak on this Mac.");
  }
}

export const computerUsePermissionHost: ComputerUsePermissionHost = {
  availability: () =>
    !hasNativeHost() ? "web" : attachedRemotely() ? "remote" : "local",
  status: () => {
    requireLocalHost();
    return invoke("computer_use_permission_status");
  },
  request: () => {
    requireLocalHost();
    return invoke("request_computer_use_permissions");
  },
  openSettings: (pane) => {
    requireLocalHost();
    return invoke("open_computer_use_permission_settings", { pane });
  },
  restart: () => {
    requireLocalHost();
    return invoke("restart_app");
  },
};
