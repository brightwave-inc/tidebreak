import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { FolderGit2, GitBranch, Plus, Sparkles } from "lucide-react";

import { useApp } from "@/AppContext";
import { Button } from "@/components/ui/button";
import {
  Empty,
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
    <div
      className={cn(
        "mx-auto flex min-h-full w-full flex-col gap-8 px-6 py-8",
        showEmpty ? "max-w-5xl" : "max-w-3xl",
      )}
    >
      {!showEmpty && (
        <header>
          <h1 className="text-2xl font-medium tracking-tight">Code</h1>
          <p className="text-muted-foreground text-sm">
            Register a local git repository, then open isolated workspaces on
            it.
          </p>
        </header>
      )}
      {error && <p className="text-sm text-critical">{error}</p>}
      {showLoading && (
        <Empty role="status">
          <EmptyHeader>
            <EmptyMedia variant="icon" className="text-muted-foreground">
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
            No pinned harness is ready yet. Refresh to install the engines this
            build drives, sign in from your own terminal, then start a
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
        <div className="flex flex-1 items-center">
          <CodeRepoEmptyState onAddRepo={() => setAddOpen(true)} />
        </div>
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

const CODE_ONBOARDING_STEPS = [
  {
    icon: FolderGit2,
    title: "Add a repository",
    description: "Browse a local folder or clone from a git URL or GitHub.",
  },
  {
    icon: GitBranch,
    title: "Open a workspace",
    description: "Give the task an isolated branch and working directory.",
  },
  {
    icon: Sparkles,
    title: "Start the work",
    description: "Choose a coding agent and hand it a concrete task.",
  },
];

/**
 * The settled first-run state for code mode.
 *
 * Kept presentational so Storybook can show the page without waiting on the
 * repo catalog and harness doctor that decide when production reaches it.
 */
export function CodeRepoEmptyState({ onAddRepo }: { onAddRepo: () => void }) {
  return (
    <section
      className="grid w-full items-center gap-12 py-10 md:grid-cols-[minmax(0,1.15fr)_minmax(16rem,0.85fr)] md:gap-16 md:py-16"
      aria-labelledby="code-empty-title"
    >
      <div className="max-w-xl">
        <div className="mb-5 flex items-center gap-2 text-xs font-medium text-muted-foreground">
          <FolderGit2 aria-hidden="true" className="size-4" />
          <span>Code mode</span>
        </div>
        <h1
          id="code-empty-title"
          className="max-w-lg text-3xl leading-[1.08] font-semibold tracking-[-0.04em] text-balance sm:text-4xl"
        >
          Start with a repository
        </h1>
        <p className="mt-4 max-w-lg text-[0.95rem] leading-6 text-muted-foreground text-pretty">
          Register a local checkout or clone one from a remote. Tidebreak uses
          it to create isolated workspaces for agent tasks.
        </p>
        <div className="mt-7 flex flex-wrap items-center gap-3">
          <Button type="button" size="lg" onClick={onAddRepo}>
            <Plus aria-hidden="true" />
            Add repo
          </Button>
          <span className="text-xs text-muted-foreground">
            Local folder, git URL, or GitHub
          </span>
        </div>
      </div>

      <ol
        className="relative flex flex-col before:absolute before:top-5 before:bottom-5 before:left-[1.125rem] before:w-px before:bg-border-subtle before:content-['']"
        aria-label="How code mode starts"
      >
        {CODE_ONBOARDING_STEPS.map(({ icon: Icon, title, description }) => (
          <li key={title} className="relative flex gap-4 pb-7 last:pb-0">
            <span className="z-10 grid size-9 shrink-0 place-items-center rounded-full border border-border-subtle bg-background text-muted-foreground shadow-xs">
              <Icon aria-hidden="true" className="size-4" strokeWidth={1.75} />
            </span>
            <span className="min-w-0 pt-0.5">
              <span className="block text-sm font-medium tracking-[-0.01em]">
                {title}
              </span>
              <span className="mt-0.5 block text-[0.8rem] leading-5 text-muted-foreground text-pretty">
                {description}
              </span>
            </span>
          </li>
        ))}
      </ol>
    </section>
  );
}
