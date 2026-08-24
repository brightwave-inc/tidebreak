import { useEffect, useState } from "react";
import { FolderGit2, GitBranch, Plus, Sparkles } from "lucide-react";

import type { HarnessKind } from "../api/types";

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
import { useCodeUiStore } from "./CodeUiStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";
import { DoctorList } from "./DoctorList";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { harnessUnusableReason } from "./labels";
import { middleTruncate } from "./workspaceCards";

/**
 * `/code` home: the doctor when no engine can be started or downloaded,
 * otherwise repo registration and the registered list.
 *
 * A machine with nothing downloaded yet is not blocked: picking an engine in
 * the New Workspace dialog fetches it. The doctor only takes the page when
 * every engine is signed out or unsupported, which is the one case a reader
 * cannot resolve by starting a workspace.
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
  const { client } = useApp();
  const startNewWorkspace = useCodeUiStore((state) => state.startNewWorkspace);
  const doctor = useCodeCatalogStore((state) => state.doctor);
  const repos = useCodeCatalogStore((state) => state.repos);
  const loaded = useCodeCatalogStore((state) => state.loaded);
  const error = useCodeCatalogStore((state) => state.error);
  const refresh = useCodeCatalogStore((state) => state.refresh);
  const refreshDoctor = useCodeCatalogStore((state) => state.refreshDoctor);
  const [addOpen, setAddOpen] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const installs = useCodeUpdatesStore((state) => state.harnessInstalls);
  const reloadDoctor = useCodeCatalogStore((state) => state.reloadDoctor);

  useEffect(() => {
    void refresh(client);
  }, [client, refresh]);

  // Startable now, or one download away. Either way the reader can get to
  // work from here, so the register form is what the page owes them.
  const usable =
    doctor?.harnesses.some((entry) => !harnessUnusableReason(entry)) ?? false;
  const showRepos = loaded && repos.length > 0;
  const showEmpty = loaded && repos.length === 0 && usable;
  const showDoctor = Boolean(doctor && !usable);
  // Repos resolve before the doctor. Until one of the three settled
  // bodies can render, keep this slot filled so the empty state does not pop in.
  const showLoading = !showRepos && !showEmpty && !showDoctor && !error;

  async function install(kind: HarnessKind) {
    try {
      const snapshot = await client.startHarnessInstall(kind, true);
      useCodeUpdatesStore
        .getState()
        .apply({ type: "harness_install", install: snapshot });
    } catch {
      // Create still reports why, with the reason the server gave.
    }
  }

  // Pick up an engine a download just put on disk.
  useEffect(() => {
    if (!Object.values(installs).some((item) => item?.phase === "ready"))
      return;
    void reloadDoctor(client).catch(() => {});
  }, [client, installs, reloadDoctor]);

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
          <h2 className="text-lg font-semibold">Set up a coding engine</h2>
          <p className="text-muted-foreground text-sm">
            No engine can start yet. Sign in to one from your own terminal, then
            re-check.
          </p>
          <DoctorList
            report={doctor}
            onRefresh={() => void onRefresh()}
            refreshing={refreshing}
            onInstall={(kind) => void install(kind)}
            installs={installs}
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
            <div>
              <h2 className="text-lg font-semibold">Repos</h2>
              <p className="text-muted-foreground text-sm">
                Pick one to open a workspace on it.
              </p>
            </div>
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
                  aria-label={`New workspace on ${repo.display_name}`}
                  onClick={() => startNewWorkspace(repo.id)}
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
        <p className="mt-4 max-w-lg text-md leading-6 text-muted-foreground text-pretty">
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
              <span className="mt-0.5 block text-xs leading-5 text-muted-foreground text-pretty">
                {description}
              </span>
            </span>
          </li>
        ))}
      </ol>
    </section>
  );
}
