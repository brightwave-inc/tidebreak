import { useMemo } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "@/AppContext";
import { RouteFrame } from "@/RouteFrame";
import { AppSidebar } from "@/sidebar/AppSidebar";
import { PluginDetailView } from "./PluginDetailView";
import { pluginsApisFromClient } from "./pluginsApis";
import { PluginsView } from "./PluginsView";
import { usePluginCatalog } from "./usePluginCatalog";

/**
 * The Plugins library as a full page: the catalog at `/plugins`, one bundle's
 * detail — with its member skills and their own switches — at
 * `/plugins/{slug}`.
 *
 * Both read the catalog this page owns, so a switch flipped on the list is
 * already flipped when the detail opens. Like Apps, plugins are install-wide,
 * so the library takes the whole pane rather than a tab beside a conversation.
 */
export function PluginsPage({ pluginId }: { pluginId?: string }) {
  const navigate = useNavigate();
  const { client } = useApp();
  const apis = useMemo(() => pluginsApisFromClient(client), [client]);
  const state = usePluginCatalog(apis);

  return (
    <RouteFrame sidebar={<AppSidebar />}>
      <div className="content-container flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden">
        {pluginId ? (
          <PluginDetailView
            pluginId={pluginId}
            state={state}
            loadInstructions={apis.instructions}
            onBack={() => void navigate({ to: "/plugins" })}
          />
        ) : (
          <PluginsView
            state={state}
            loadInstructions={apis.instructions}
            onOpen={(id) =>
              void navigate({ to: "/plugins/$pluginId", params: { pluginId: id } })
            }
          />
        )}
      </div>
    </RouteFrame>
  );
}
