import type { ExecConfigInfo } from "./api";

export const MANAGED_EXECUTION_DISCLOSURE =
  "Code execution runs in an E2B or Daytona cloud sandbox on this device. Files staged for a run leave this machine and are uploaded to that provider.";

/**
 * Whether this host has no native execution path and therefore must use a
 * managed provider. The server owns the platform decision; the renderer only
 * interprets its structured Local-provider capability row.
 */
export function requiresManagedExecutionDisclosure(
  providers: ExecConfigInfo["providers"] | null | undefined,
): boolean {
  const local = providers?.find((row) => row.provider === "local");
  return (
    local?.available === false &&
    local.unavailable_reason === "unsupported_platform"
  );
}
