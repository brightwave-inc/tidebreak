import { useCodeCatalogStore } from "./CodeCatalogStore";
import { activateCodeClientGeneration } from "./CodeClientGeneration";
import { resetCodeDeliveryHostState } from "./CodeDeliveryStore";
import { resetCodeSessionRegistry } from "./CodeSessionRegistry";
import { resetCodeUiHostState } from "./CodeUiStore";
import {
  activateCodeCloneClient,
  disconnectCodeUpdates,
  useCodeUpdatesStore,
} from "./CodeUpdatesStore";
import { resetCodeSubscriptionUsageStore } from "./useCodeSubscriptionUsage";

/**
 * Move every host-scoped Code store to one ApiClient before routes can mount.
 */
export function activateCodeClient(client: object): number {
  const activation = activateCodeClientGeneration(client);
  if (!activation.changed) return activation.generation;

  disconnectCodeUpdates();
  resetCodeSessionRegistry();
  useCodeCatalogStore.getState().reset();
  resetCodeDeliveryHostState();
  resetCodeSubscriptionUsageStore();
  resetCodeUiHostState();
  useCodeUpdatesStore.getState().reset();
  activateCodeCloneClient(client);
  return activation.generation;
}
