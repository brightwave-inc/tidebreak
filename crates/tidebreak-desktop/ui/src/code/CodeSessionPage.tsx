import { RouteFrame } from "@/RouteFrame";
import { CodeSessionPane } from "./workspace/CodeSessionPane";
import { CodeSidebar } from "./CodeSidebar";
import { useApp } from "@/AppContext";
import { useCodeCatalogStore } from "./CodeCatalogStore";

/**
 * A repository-less internal-engine session opened directly (decision 0048
 * step 5, decision 0086).
 *
 * The same pane owns a workspace session; the only differences are that there
 * is no worktree to mention files from, and the live digest lives in the
 * conversations-without-workspace map. Viewers get the read-only explanation
 * and contributors get the composer, exactly as on a workspace session.
 */
export function CodeSessionPage({ sessionId }: { sessionId: string }) {
  const { client, models, defaultModelKey } = useApp();
  const catalogModels = useCodeCatalogStore((state) => state.models);
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <div className="content-container flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden">
        <div className="chat-pane flex min-h-0 min-w-0 flex-1 flex-col">
          <CodeSessionPane
            key={sessionId}
            session={
              // A rendered internal session arrives through the digest row;
              // this compact fallback lets the pane mount before the summary
              // answers with the authoritative snapshot.
              {
                id: sessionId,
                workspace_id: null,
                kind: "interactive",
                harness_kind: "internal",
                permission_mode: "ask",
                fast_mode: false,
                lifecycle: "idle",
                attention: { state: { type: "idle" }, source: "lifecycle" },
                unrecognized_event_count: 0,
                visibility: "private",
                created_at: new Date().toISOString(),
                execution_location: "machine",
              } as import("../api/types").CodeSessionSnapshot
            }
            workspaceId={null}
            client={client}
            catalogModels={catalogModels}
            defaultModelKey={defaultModelKey}
            disabled={false}
          />
        </div>
      </div>
    </RouteFrame>
  );
}
