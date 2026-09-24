import { invoke } from "@tauri-apps/api/core";

import { hasNativeHost } from "./host";

/** Where the command can be installed: for this account, or for every account. */
export type CliLocation = "user" | "system";

/** What is at one install location, as `cli_command.rs` reports it. */
export type CliLinkState =
  | { state: "missing" }
  | { state: "installed" }
  | { state: "stale"; target: string }
  | { state: "foreign"; target: string | null };

export type CliLink = CliLinkState & {
  /** The link's full path. */
  path: string;
  /** Whether a new terminal looks in the link's folder; `null` when unknown. */
  onPath: boolean | null;
};

export type CliCommandStatus =
  | {
      status: "unavailable";
      reason: "unsupported" | "not_bundled" | "temporary_location";
    }
  | {
      status: "available";
      /** The bundled command every link points at. */
      command: string;
      user: CliLink;
      system: CliLink;
      /** What `tidebreak` runs in a new terminal, when anything. */
      resolved: string | null;
    };

export type CliCommandChange = {
  location: CliLocation;
  outcome: "created" | "updated" | "unchanged" | "removed" | "absent";
  /** The install created the link's folder. */
  folderCreated: boolean;
  status: CliCommandStatus;
};

export interface CliCommandHost {
  /** Whether this window runs inside the desktop app. */
  available(): boolean;
  status(): Promise<CliCommandStatus>;
  install(location: CliLocation): Promise<CliCommandChange>;
  uninstall(location: CliLocation): Promise<CliCommandChange>;
}

/**
 * The desktop's native commands for the `tidebreak` command. They act on the
 * Mac this window runs on, whichever machine the window is attached to: the
 * link points into this copy of the app.
 */
export const cliCommandHost: CliCommandHost = {
  available: hasNativeHost,
  status: () => invoke("cli_command_status"),
  install: (location) => invoke("install_cli_command", { location }),
  uninstall: (location) => invoke("uninstall_cli_command", { location }),
};
