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
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { toast } from "sonner";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { AddRepoPalette } from "./AddRepoPalette";
import { useCodeUiStore } from "./CodeUiStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";
import { DoctorList } from "./DoctorList";
import { RepositorySettingsDialog } from "./RepositorySettingsDialog";
import { openEngineSignIn } from "./EngineSignIn";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { harnessNeedsNoSignIn, workspaceHarnesses } from "./labels";
import { middleTruncate } from "./workspaceCards";
import { PaneDragBand } from "@/WindowDragStrip";

/**
 * `/code` home: the doctor until some engine can run a first turn, then repo
 * registration and the registered list.
 *
 * Downloading an engine is not the whole setup. The pin Tidebreak downloads
 * still needs its own sign-in, and a reader sent straight to a workspace
 * found that out when the first turn failed. So the doctor takes the page
 * until an engine is signed in, or could be without a sign-in: one whose
 * sign-in Tidebreak could not confirm, or one a gateway carries.
 */

export function CodeHome() {
  return (
    <div className="content-container relative min-h-0 w-full min-w-0 flex-1 overflow-auto">
      <PaneDragBand />
      <CodeHomeBody />
    </div>
  );
}

function CodeHomeBody() {
  const { client } = useApp();
  const startNewWorkspace = useCodeUiStore((state) => state.startNewWorkspace);
  const [settingsRepo, setSettingsRepo] = useState<{
    id: string;
    label: string;
  } | null>(null);
  const doctor = useCodeCatalogStore((state) => state.doctor);
  const doctorError = useCodeCatalogStore((state) => state.doctorError);
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

  // Nothing but a download stands between some engine and its first turn,
  // so the register form is what the page owes the reader.
  const usable = workspaceHarnesses(doctor?.harnesses ?? []).some(
    harnessNeedsNoSignIn,
  );
  const showRepos = loaded && repos.length > 0;
  const showEmpty = loaded && repos.length === 0 && usable;
  const showDoctor = Boolean(doctor && !usable);
  // Repos resolve before the doctor. Until one of the three settled
  // bodies can render, keep this slot filled so the empty state does not pop in.
  const showLoading =
    !showRepos && !showEmpty && !showDoctor && !doctorError && !error;

  async function install(kind: HarnessKind) {
    try {
      const snapshot = await client.startHarnessInstall(kind, true);
      useCodeUpdatesStore
        .getState()
        .apply({ type: "harness_install", install: snapshot });
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not start the download"));
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
    } catch {
      // The existing warning remains the useful result of a failed re-check.
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
      {error && (
        <div className="notice-surface notice-critical flex items-center justify-between gap-3 rounded-lg border px-3 py-2 text-sm">
          <span>{error}</span>
          <Button
            type="button"
            size="xs"
            variant="outline"
            onClick={() => void refresh(client)}
          >
            Try again
          </Button>
        </div>
      )}
      {doctorError && (
        <div className="notice-surface notice-warning flex items-center justify-between gap-3 rounded-lg border px-3 py-2 text-sm">
          <span>The coding engine check did not answer: {doctorError}</span>
          <Button
            type="button"
            size="xs"
            variant="outline"
            onClick={() => void onRefresh()}
          >
            {refreshing ? "Re-checking…" : "Re-check"}
          </Button>
        </div>
      )}
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
          {/* The list's own verdict says what is left: a download, a
              sign-in, or both. */}
          <DoctorList
            report={doctor}
            onRefresh={() => void onRefresh()}
            refreshing={refreshing}
            onInstall={(kind) => void install(kind)}
            installs={installs}
            onSignIn={openEngineSignIn}
          />
        </section>
      )}
      {doctorError && repos.length === 0 && (
        <div className="flex flex-1 items-center">
          <CodeRepoEmptyState onAddRepo={() => setAddOpen(true)} />
        </div>
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
              <li key={repo.id} className="flex min-w-0 items-center gap-1">
                <button
                  type="button"
                  className={cn(
                    "hover:bg-muted flex min-w-0 flex-1 cursor-pointer items-baseline gap-2 rounded-md px-3 py-2 text-left text-sm",
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
                <Button
                  type="button"
                  size="xs"
                  variant="outline"
                  onClick={() =>
                    setSettingsRepo({
                      id: repo.id,
                      label: repo.display_name,
                    })
                  }
                >
                  Settings
                </Button>
              </li>
            ))}
          </ul>
        </section>
      )}
      <AddRepoPalette open={addOpen} onOpenChange={setAddOpen} />
      <RepositorySettingsDialog
        open={settingsRepo !== null}
        onOpenChange={(open) => {
          if (!open) setSettingsRepo(null);
        }}
        client={client}
        repoId={settingsRepo?.id ?? null}
        repoLabel={settingsRepo?.label ?? ""}
        onSaved={(repo) => useCodeCatalogStore.getState().upsertRepo(repo)}
      />
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
            <span className="z-10 grid size-9 shrink-0 place-items-center rounded-full border border-border-subtle bg-background text-muted-foreground">
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
