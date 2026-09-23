import type { ConfirmOptions } from "@/components/ConfirmDialog";
import { hasLocalHostAuthority, leaveProvisionedGateway } from "./host";

/**
 * How a surface leaves a gateway the person connected through a link.
 *
 * The native shell runs it: policy is never written over HTTP. Surfaces take
 * it as a prop so a story or a test can render the control without a shell
 * behind it.
 */
export type GatewayLeaveControl = {
  /**
   * Whether this window can leave the gateway: the desktop app, working on
   * this computer. A browser tab has no shell to ask, and a window attached
   * to another machine shows that machine's policy, not this computer's.
   */
  available: boolean;
  /** Delete the provisioned policy that names `gatewayUrl`. */
  leave: (gatewayUrl: string) => Promise<unknown>;
};

export function nativeGatewayLeave(): GatewayLeaveControl {
  return {
    available: hasLocalHostAuthority(),
    leave: leaveProvisionedGateway,
  };
}

/** The host a gateway URL names, for a sentence. */
export function gatewayHost(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/** The one confirmation in front of leaving, wherever the control sits. */
export function leaveGatewayConfirmation(gatewayUrl: string): ConfirmOptions {
  return {
    title: "Leave this gateway?",
    description: `Tidebreak signs out of ${gatewayHost(gatewayUrl)} and stops being managed by it. Your own provider keys and settings come back. You can connect again later from the gateway's page.`,
    confirmLabel: "Leave gateway",
    destructive: true,
  };
}
