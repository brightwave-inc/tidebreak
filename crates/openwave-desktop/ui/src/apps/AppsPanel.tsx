import { useMemo } from "react";

import { useApp } from "@/AppContext";
import { PanelFrame } from "@/panel/PanelFrame";
import type { PanelContent } from "@/panel/panelTypes";
import { usePanelNav } from "@/panel/usePanelNav";
import { AppDetailView } from "./AppDetailView";
import { AppsView } from "./AppsView";
import { appsApisFromClient } from "./appsApis";

/**
 * The `apps` panel: the library list, or — addressed `apps.{appId}` — one
 * app's detail with the open flow. Hosted by the home route (the library's
 * primary home) and available beside a conversation like any other
 * navigation panel.
 */
export function AppsPanel({
  panel,
  position,
}: {
  panel: Extract<PanelContent, { type: "apps" }>;
  position: "left" | "right";
}) {
  const { client } = useApp();
  const { openPanel } = usePanelNav();
  const apis = useMemo(() => appsApisFromClient(client), [client]);

  return (
    <PanelFrame position={position} spaceBetween>
      {panel.appId ? (
        <AppDetailView
          appId={panel.appId}
          apis={apis}
          onBack={() => openPanel({ type: "apps" })}
        />
      ) : (
        <AppsView
          apis={apis}
          onOpen={(appId) => openPanel({ type: "apps", appId })}
        />
      )}
    </PanelFrame>
  );
}
