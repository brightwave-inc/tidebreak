import { invoke } from "@tauri-apps/api/core";

import { hasNativeHost } from "./host";

/**
 * The known reasons the local server did not start, as the desktop names
 * them (`tidebreak_server::boot_failure`).
 */
export type BootFailureKind =
  | "instance_lock"
  | "newer_version"
  | "unrecognized_data"
  | "unsupported_version"
  | "migration"
  | "disk_full"
  | "keychain"
  | "unknown";

/** Why the local server is not serving, as the boot screen needs it. */
export type LocalBootFailure = {
  kind: BootFailureKind;
  /**
   * The server started and then stopped. It cannot be started again inside
   * this process, so the screen offers a restart instead of another try.
   */
  stopped: boolean;
  /** The data folder, which still holds every conversation. */
  dataDir: string;
};

/**
 * What the shell knows about a local boot that failed, or `null` when there
 * is nothing to add: outside the desktop, or when the shell cannot say.
 */
export async function localBootFailure(): Promise<LocalBootFailure | null> {
  if (!hasNativeHost()) return null;
  try {
    return (
      (await invoke<LocalBootFailure | null>("local_boot_failure")) ?? null
    );
  } catch {
    return null;
  }
}

/**
 * Run the local server's boot again. Boot used to run once per launch, so
 * Try again read the same failure back however the cause had changed.
 */
export async function retryLocalBoot(): Promise<void> {
  if (!hasNativeHost()) return;
  await invoke("retry_server_boot");
}
