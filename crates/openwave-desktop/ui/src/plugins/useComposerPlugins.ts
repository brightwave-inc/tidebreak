import { useMemo } from "react";

import type { ApiClient } from "@/api";
import type { ComposerPlugins } from "@/ComposerToolsMenu";
import { pluginsApisFromClient } from "./pluginsApis";
import { usePluginCatalog } from "./usePluginCatalog";

/**
 * The installed bundles, as the composer's tools menu wants them.
 *
 * Picking one from the composer is a shortcut, not a per-chat setting: a
 * bundle being on is still a property of this installation, so engaging a
 * bundle that is off turns it on the same way its switch on the Plugins page
 * would. Nothing is returned while the catalog is loading or after it failed —
 * the menu shows no section rather than reporting on a library the reader did
 * not ask about.
 */
export function useComposerPlugins(
  client: ApiClient,
): ComposerPlugins | undefined {
  const apis = useMemo(() => pluginsApisFromClient(client), [client]);
  const { catalog, setEnabled } = usePluginCatalog(apis);
  const plugins = catalog?.plugins ?? [];
  if (plugins.length === 0) return undefined;
  return {
    items: plugins,
    onSelect: (plugin) => {
      if (plugin.enabled) return;
      setEnabled({ plugins: { [plugin.name]: true }, skills: {} });
    },
  };
}
