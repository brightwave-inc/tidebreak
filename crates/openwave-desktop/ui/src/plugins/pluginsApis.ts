import type { ApiClient, PluginCatalog, PluginEnableUpdate } from "@/api";

/**
 * Everything the Plugins library calls on the server, as one injectable
 * object — the {@link import("@/apps/appsApis").AppsApis} pattern, so the list,
 * the detail, and the toggle round trip are drivable in tests without a
 * network.
 */
export type PluginsApis = {
  list(): Promise<PluginCatalog>;
  setEnabled(update: PluginEnableUpdate): Promise<PluginCatalog>;
};

export function pluginsApisFromClient(client: ApiClient): PluginsApis {
  return {
    list: () => client.listPlugins(),
    setEnabled: (update) => client.setPluginsEnabled(update),
  };
}
