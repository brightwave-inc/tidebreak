import type { useNavigate } from "@tanstack/react-router";
import { useEffect, useRef } from "react";

import type { ModelInfo } from "./api";
import type { ComposerSendBlocker } from "./Composer";
import type { ProviderSetupTarget } from "./ProviderSetupCard";

type Navigate = ReturnType<typeof useNavigate>;

// Settings sections are registered from a runtime table, so TanStack's
// generated route union contains `/settings` but not each literal child.
const PROVIDERS_PATH: string = "/settings/providers";
const GATEWAY_PATH: string = "/settings/gateway";

/**
 * Whether the catalog has loaded and nothing in it can run. While the first
 * read is in flight the list is empty for another reason, and saying "set
 * up a provider" then would flash on every launch.
 */
export function noRunnableModel(
  models: readonly ModelInfo[],
  catalogLoaded: boolean | undefined,
): boolean {
  return catalogLoaded !== false && !models.some((model) => model.available);
}

/** Open Settings → Providers on the card a setup choice names. */
export function openProviderSetup(
  navigate: Navigate,
  target: ProviderSetupTarget,
): void {
  void navigate({
    to: PROVIDERS_PATH,
    search: target.provider
      ? {
          provider: target.provider,
          ...(target.focusCredential ? { focus: "credential" } : {}),
        }
      : {},
  });
}

/** Open the Model Gateway settings, where a managed profile's models come from. */
export function openGatewaySettings(navigate: Navigate): void {
  void navigate({ to: GATEWAY_PATH });
}

/**
 * Why a Work composer cannot send when no model can run, with the one action
 * that fixes it. A managed profile has no key to add, so it points at the
 * gateway instead.
 */
export function noModelSendBlocker(
  noModelCanRun: boolean,
  managed: boolean,
  navigate: Navigate,
): ComposerSendBlocker | null {
  if (!noModelCanRun) return null;
  if (managed) {
    return {
      reason: "No model from your organization's gateway is available yet.",
      action: {
        label: "Open gateway settings",
        onClick: () => openGatewaySettings(navigate),
      },
    };
  }
  return {
    reason: "Connect a model provider to send.",
    action: {
      label: "Set up a provider",
      onClick: () =>
        openProviderSetup(navigate, { provider: null, focusCredential: false }),
    },
  };
}

/** How often the catalog is read again while no model can run. */
export const SETUP_REFRESH_INTERVAL_MS = 5_000;

/**
 * While no model can run, read the catalog again whenever the window comes
 * back to the front, and every few seconds. A ChatGPT sign-in can finish in
 * the browser after the Settings panel that started it has closed, and a
 * gateway's model list can land after its sign-in, so this lifts the send
 * block as soon as a model can run, without a restart. Nothing runs once a
 * model can, or while the window is hidden.
 */
export function useRefreshWhileNoModelRuns(
  noModelCanRun: boolean,
  refresh: () => Promise<void>,
  intervalMs: number = SETUP_REFRESH_INTERVAL_MS,
): void {
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  useEffect(() => {
    if (!noModelCanRun) return;
    let inFlight = false;
    const look = () => {
      if (inFlight || document.visibilityState === "hidden") return;
      inFlight = true;
      refreshRef
        .current()
        .catch(() => undefined)
        .finally(() => {
          inFlight = false;
        });
    };
    const timer = window.setInterval(look, intervalMs);
    window.addEventListener("focus", look);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("focus", look);
    };
  }, [noModelCanRun, intervalMs]);
}
