import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "@/AppContext";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { RouteFrame } from "@/RouteFrame";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { AddRepoPalette } from "./AddRepoPalette";
import { CodeSidebar } from "./CodeSidebar";
import { DoctorList } from "./DoctorList";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { isHarnessReady } from "./labels";
import { middleTruncate } from "./workspaceCards";

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
  const refreshDoctor = useCodeCatalogStore((state) => state.refreshDoctor);
  const [addOpen, setAddOpen] = useState(false);
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => {
    void refresh(client);
  }, [client, refresh]);

  const ready = doctor?.harnesses.some(isHarnessReady) ?? false;
  const showRepos = loaded && repos.length > 0;
  const showEmpty = loaded && repos.length === 0 && ready;
  const showDoctor = Boolean(doctor && !ready);
  // Repos resolve before the doctor. Until one of the three settled
  // bodies can render, keep this slot filled so the empty state does not pop in.
  const showLoading = !showRepos && !showEmpty && !showDoctor && !error;

  async function onRefresh() {
    setRefreshing(true);
    try {
      await refreshDoctor(client);
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
      {showLoading && (
        <Empty role="status">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <Spinner aria-hidden="true" />
            </EmptyMedia>
            <EmptyTitle>Loading…</EmptyTitle>
          </EmptyHeader>
        </Empty>
      )}
      {showDoctor && doctor && (
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
      {showEmpty && (
        // Nothing is registered, so the page has exactly one thing to say.
        // A "Repos" heading over an empty list, under a second heading that
        // repeats the same instruction, is two sections carrying one message.
        <Empty>
          <EmptyHeader>
            <EmptyTitle>No repos yet</EmptyTitle>
            <EmptyDescription>
              Browse a local folder, or clone from a git URL or GitHub.
            </EmptyDescription>
          </EmptyHeader>
          <EmptyContent>
            <Button type="button" onClick={() => setAddOpen(true)}>
              Add repo
            </Button>
          </EmptyContent>
        </Empty>
      )}
      {showRepos && (
        <section className="flex flex-col gap-3">
          <div className="flex items-center justify-between gap-3">
            <h2 className="text-lg font-semibold">Repos</h2>
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={() => setAddOpen(true)}
            >
              Add repo
            </Button>
          </div>
          <ul className="flex flex-col gap-1">
            {repos.map((repo) => (
              <li key={repo.id}>
                <button
                  type="button"
                  className={cn(
                    "hover:bg-muted flex w-full cursor-pointer items-baseline gap-2 rounded-md px-3 py-2 text-left text-sm",
                    FOCUS_RING,
                    HOVER_TINT,
                  )}
                  onClick={() =>
                    void navigate({
                      to: "/code/r/$repoId",
                      params: { repoId: repo.id },
                    })
                  }
                >
                  <span className="min-w-0 shrink truncate font-medium">
                    {repo.display_name}
                  </span>
                  {/* The tail of a path is what tells two checkouts apart. */}
                  <span
                    className="text-muted-foreground min-w-0 truncate font-mono text-xs"
                    title={repo.root_path}
                  >
                    {middleTruncate(repo.root_path, 56)}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
      <AddRepoPalette open={addOpen} onOpenChange={setAddOpen} />
    </div>
  );
}
