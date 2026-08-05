import { useMemo } from "react";

import { useApp } from "@/AppContext";
import { PanelFrame } from "@/panel/PanelFrame";
import type { PanelContent } from "@/panel/panelTypes";
import { usePanelNav } from "@/panel/usePanelNav";
import { PluginDetailView } from "./PluginDetailView";
import { pluginsApisFromClient } from "./pluginsApis";
import { PluginsView } from "./PluginsView";
import { usePluginCatalog } from "./usePluginCatalog";

/**
 * The `plugins` panel: the library list, or — addressed `plugins.{slug}` — one
 * bundle's detail. Both read the catalog this panel owns, so a switch flipped
 * on the list is already flipped when the detail opens.
 */
export function PluginsPanel({
  panel,
}: {
  panel: Extract<PanelContent, { type: "plugins" }>;
}) {
  const { client } = useApp();
  const { openPanel } = usePanelNav();
  const apis = useMemo(() => pluginsApisFromClient(client), [client]);
  const state = usePluginCatalog(apis);

  return (
    <PanelFrame spaceBetween>
      {panel.pluginId ? (
        <PluginDetailView
          pluginId={panel.pluginId}
          state={state}
          onBack={() => openPanel({ type: "plugins" })}
        />
      ) : (
        <PluginsView
          state={state}
          onOpen={(pluginId) => openPanel({ type: "plugins", pluginId })}
        />
      )}
    </PanelFrame>
  );
}
