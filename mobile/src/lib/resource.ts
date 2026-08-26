import { sha256Hex } from "./crypto";

export const RESOURCE_CONTROL = "control";

/** Canonical `tidebreak:<sha256(canonical_public_url) hex>` derivation. */
export function tidebreakMachineResource(canonicalPublicUrl: string): string {
  return `tidebreak:${sha256Hex(canonicalPublicUrl)}`;
}

export function isAllowedResource(resource: string): boolean {
  return resource === RESOURCE_CONTROL || resource.startsWith("tidebreak:");
}

export function assertResourceEcho(
  derived: string,
  echoed: string,
): void {
  if (derived !== echoed) {
    throw new Error(
      "The machine advertised a resource that does not match the URL you entered.",
    );
  }
}
