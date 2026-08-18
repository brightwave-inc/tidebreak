import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "@/AppContext";
import { Button } from "@/components/ui/button";
import { RouteFrame } from "@/RouteFrame";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { AddRepoPalette } from "./AddRepoPalette";
import { CodeSidebar } from "./CodeSidebar";
import { DoctorList } from "./DoctorList";
import { isHarnessReady } from "./labels";

/**
 * `/code` home: the doctor when no engine is usable, otherwise repo
 * registration and the registered list.
 *
 * A reader who has not installed or signed in to a harness cannot start a
 * workspace, so the empty state is the remediation the doctor already wrote
 * rather than a blank register form.
 */

export function CodeHome() {
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-auto">
        <CodeHomeBody />
      </div>
    </RouteFrame>
  );
}

function CodeHomeBody() {
  const navigate = useNavigate();
  const { client } = useApp();
  const doctor = useCodeCatalogStore((state) => state.doctor);
  const repos = useCodeCatalogStore((state) => state.repos);
  const loaded = useCodeCatalogStore((state) => state.loaded);
  const error = useCodeCatalogStore((state) => state.error);
  const refresh = useCodeCatalogStore((state) => state.refresh);
  const [addOpen, setAddOpen] = useState(false);
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => {
    void refresh(client);
  }, [client, refresh]);

  const ready = doctor?.harnesses.some(isHarnessReady) ?? false;

  async function onRefresh() {
    setRefreshing(true);
    try {
      await refresh(client);
    } finally {
      setRefreshing(false);
    }
  }

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-8 px-6 py-8">
      <div>
        <h1 className="text-2xl font-medium tracking-tight">Code</h1>
        <p className="text-muted-foreground text-sm">
          Register a local git repository, then open isolated workspaces on it.
        </p>
      </div>
      {error && <p className="text-sm text-critical">{error}</p>}
      {!loaded && <p className="text-muted-foreground text-sm">Loading…</p>}
      {doctor && !ready && (
        <section className="flex flex-col gap-3">
          <h2 className="text-lg font-semibold">Install a coding harness</h2>
          <p className="text-muted-foreground text-sm">
            No pinned harness is ready yet. Refresh to install the engines
            this build drives, sign in from your own terminal, then start a
            workspace.
          </p>
          <DoctorList
            report={doctor}
            onRefresh={() => void onRefresh()}
            refreshing={refreshing}
          />
        </section>
      )}
      {ready && (
        <>
          <section className="flex flex-col gap-3">
            <h2 className="text-lg font-semibold">Add a repo</h2>
            <p className="text-muted-foreground text-sm">
              Browse a local folder, or clone from a git URL or GitHub.
            </p>
            <Button type="button" className="self-start" onClick={() => setAddOpen(true)}>
              Add repo
            </Button>
          </section>
          <section className="flex flex-col gap-3">
            <h2 className="text-lg font-semibold">Repos</h2>
            {repos.length === 0 ? (
              <p className="text-muted-foreground text-sm">None registered yet.</p>
            ) : (
              <ul className="flex flex-col gap-1">
                {repos.map((repo) => (
                  <li key={repo.id}>
                    <button
                      type="button"
                      className="hover:bg-muted w-full rounded-md px-3 py-2 text-left text-sm"
                      onClick={() =>
                        void navigate({
                          to: "/code/r/$repoId",
                          params: { repoId: repo.id },
                        })
                      }
                    >
                      <span className="font-medium">{repo.display_name}</span>
                      <span className="text-muted-foreground ml-2 font-mono text-xs">
                        {repo.root_path}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </>
      )}
      <AddRepoPalette open={addOpen} onOpenChange={setAddOpen} />
    </div>
  );
}
