import type { ExecConfigInfo } from "./api";
import { hostMachineLabel } from "./remoteMachine";

/**
 * Where code actually runs when the host has no native sandbox, and what
 * leaves with it. Named at read time rather than fixed at module load: the
 * files staged for a run leave whichever machine this window works on.
 */
export function managedExecutionDisclosure(): string {
  const host = hostMachineLabel();
  return `Code execution runs in an E2B or Daytona cloud sandbox rather than on ${host}. Files staged for a run leave ${host} and are uploaded to that provider.`;
}

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
