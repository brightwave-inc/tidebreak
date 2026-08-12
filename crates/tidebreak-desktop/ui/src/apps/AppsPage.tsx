import { useMemo } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "@/AppContext";
import { RouteFrame } from "@/RouteFrame";
import { AppSidebar } from "@/sidebar/AppSidebar";
import { AppDetailView } from "./AppDetailView";
import { AppsView } from "./AppsView";
import { appsApisFromClient } from "./appsApis";

/**
 * The Apps library as a full page: the list at `/apps`, one app's detail at
 * `/apps/{appId}`.
 *
 * Apps are install-wide — they outlive every conversation — so the library
 * takes the whole pane with the rail beside it, like the inbox, rather than
 * opening as a tab inside someone's conversation.
 */
export function AppsPage({ appId }: { appId?: string }) {
  const navigate = useNavigate();
  const { client } = useApp();
  const apis = useMemo(() => appsApisFromClient(client), [client]);

  return (
    <RouteFrame sidebar={<AppSidebar />}>
      <div className="content-container flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden">
        {appId ? (
          <AppDetailView
            key={appId}
            appId={appId}
            apis={apis}
            onBack={() => void navigate({ to: "/apps" })}
          />
        ) : (
          <AppsView
            apis={apis}
            onOpen={(id) => void navigate({ to: "/apps/$appId", params: { appId: id } })}
          />
        )}
      </div>
    </RouteFrame>
  );
}
