import { sha256Hex } from "./crypto";

export const RESOURCE_CONTROL = "control";

/** The gateway console's versioned admin and observability reads. */
export const RESOURCE_CONTROL_PLANE = "control_plane";

/** Canonical `tidebreak:<sha256(canonical_public_url) hex>` derivation. */
export function tidebreakMachineResource(canonicalPublicUrl: string): string {
  return `tidebreak:${sha256Hex(canonicalPublicUrl)}`;
}

/** The owner-scoped sandbox verbs for one runtime slug. */
export function runtimeResource(slug: string): string {
  return `runtime:${slug}`;
}

/**
 * The resources this client will ever ask a gateway to mint.
 *
 * The list is the union of the two apps being merged (epic #3398): the phone's
 * own `control` + `tidebreak:<digest>`, and the console's `control_plane` +
 * `runtime:<slug>`. It bounds what a bug or a hostile response can talk this
 * client into requesting — inference and MCP credentials stay unreachable —
 * while the gateway's own client registration remains the real boundary. A
 * gateway that has not been widened yet (brightwave-inc/model-gateway#2044)
 * refuses the console resources with `invalid_resource`, which `TokenStore`
 * surfaces as a `ResourceRefusedError` rather than a sign-out.
 */
export function isAllowedResource(resource: string): boolean {
  return (
    resource === RESOURCE_CONTROL ||
    resource === RESOURCE_CONTROL_PLANE ||
    resource.startsWith("tidebreak:") ||
    resource.startsWith("runtime:")
  );
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
