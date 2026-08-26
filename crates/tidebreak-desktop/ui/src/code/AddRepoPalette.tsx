import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import { ChevronDown, Folder, GitBranch, Link2 } from "lucide-react";
import { toast } from "sonner";

import type {
  CodeCloneDefaults,
  CodeCloneJobSnapshot,
  CodeGithubRepository,
  CodeRepoSources,
} from "../api/types";
import { useApp } from "@/AppContext";
import type { OptionRow } from "@/components/OptionListbox";
import { Button } from "@/components/ui/button";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Progress } from "@/components/ui/progress";
import {
  attachedRemotely,
  hasLocalHostAuthority,
  pickCodeDirectory,
} from "@/host";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { usesCommandModifier } from "@/ShellShortcuts";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { GithubAvatar } from "./GithubAvatar";
import { useCodeUiStore } from "./CodeUiStore";
import { STATUS_TEXT } from "./statusTone";
import {
  activateCodeCloneClient,
  codeClientGeneration,
  forgetCodeClone,
  latestCodeClone,
  reconcileCodeClone,
  setCodeCloneBackground,
  takeSelectedCodeClone,
  trackCodeClone,
  useCodeUpdatesStore,
  type CodeCloneRequest,
  type ResumableCodeClone,
} from "./CodeUpdatesStore";

type Stage = "sources" | "local" | "git_url" | "github" | "progress";
type SourceKey = "local" | "git_url" | "github";

type PaletteOpening = {
  id: number;
  clientGeneration: number;
  active: boolean;
  controller: AbortController;
};

type CloneHandoff = {
  openingId: number;
  clientGeneration: number;
  jobId: string;
  armed: boolean;
};

const CLONE_POLL_INTERVAL_MS = 1_500;

function isAuthorizationFailure(error: unknown): boolean {
  if (
    error &&
    typeof error === "object" &&
    "status" in error &&
    ((error as { status?: unknown }).status === 401 ||
      (error as { status?: unknown }).status === 403)
  ) {
    return true;
  }
  return error instanceof Error && /\b(?:401|403)\b/.test(error.message);
}

function isAbortFailure(error: unknown): boolean {
  return error instanceof DOMException
    ? error.name === "AbortError"
    : Boolean(
        error &&
          typeof error === "object" &&
          "name" in error &&
          (error as { name?: unknown }).name === "AbortError",
      );
}

const SOURCES: OptionRow[] = [
  {
    key: "local",
    label: "Local folder",
    description: "Browse a folder on disk",
    icon: Folder,
  },
  {
    key: "git_url",
    label: "Git URL",
    description: "Clone from a remote URL",
    icon: Link2,
  },
  {
    key: "github",
    label: "GitHub repository",
    description: "Clone owner/repo",
    icon: GitBranch,
  },
];

/**
 * Keyboard-first dialog for registering or cloning a repo.
 *
 * Stages stay inside one dialog: source list, a small form, then clone
 * progress. Backspace returns a stage; Escape closes.
 */
export function AddRepoPalette({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { client } = useApp();
  const upsertRepo = useCodeCatalogStore((state) => state.upsertRepo);
  const cloneJobs = useCodeUpdatesStore((state) => state.cloneJobs);
  const cloneReadErrors = useCodeUpdatesStore((state) => state.cloneReadErrors);
  const selectedClone = useCodeUpdatesStore((state) => state.selectedClone);
  const [stage, setStage] = useState<Stage>("sources");
  const [query, setQuery] = useState("");
  const [path, setPath] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [url, setUrl] = useState("");
  const [github, setGithub] = useState("");
  const [parentDir, setParentDir] = useState("");
  const [cloneName, setCloneName] = useState("");
  const [loadedDefaults, setLoadedDefaults] = useState<{
    clientGeneration: number;
    value: CodeCloneDefaults;
  } | null>(null);
  const [defaultsProbeFailed, setDefaultsProbeFailed] = useState(false);
  const [sources, setSources] = useState<CodeRepoSources | null>(null);
  const [sourcesProbeFailed, setSourcesProbeFailed] = useState(false);
  const [githubRepos, setGithubRepos] = useState<CodeGithubRepository[] | null>(
    null,
  );
  const [githubListFailed, setGithubListFailed] = useState(false);
  const [probeBusy, setProbeBusy] = useState<
    "sources" | "github" | "defaults" | null
  >(null);
  const [jobId, setJobId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [handoffBusy, setHandoffBusy] = useState(false);
  const [handoffError, setHandoffError] = useState<string | null>(null);
  const openingSequence = useRef(0);
  const openingRef = useRef<PaletteOpening | null>(null);
  const handoffRef = useRef<CloneHandoff | null>(null);
  const activeJobIdRef = useRef<string | null>(null);
  const autoAttemptedJobs = useRef(new Set<string>());
  const destinationEditVersion = useRef(0);

  const job = jobId ? cloneJobs[jobId] : undefined;
  const readError = jobId ? cloneReadErrors[jobId] : undefined;
  const defaults =
    loadedDefaults?.clientGeneration === codeClientGeneration(client)
      ? loadedDefaults.value
      : null;
  // Whether this window can name a path the machine would resolve. Browsing
  // is the host's, resolving is the machine's, and they are the same computer
  // only when the window is not attached elsewhere.
  const canBrowse = hasLocalHostAuthority();
  const attached = attachedRemotely();
  const command = useMemo(() => usesCommandModifier(navigator.userAgent), []);
  // The machine places clones itself when its operator configured a
  // destination, which is what lets someone who cannot see its filesystem
  // clone at all. Until the probe answers, assume it does not, so a desktop
  // working on its own machine never loses the field it has always had.
  const machineChoosesDestination = sources?.chooses_destination === true;
  const offered = useMemo(() => {
    // Before the probe answers, offer everything: the machine is the
    // authority, and a dialog that showed nothing while asking would read as
    // a broken dialog rather than a pending one.
    if (!sources) {
      return attached ? SOURCES.filter((row) => row.key !== "local") : SOURCES;
    }
    const available = new Map(
      sources.sources.map((source) => [source.kind, source] as const),
    );
    return SOURCES.filter((row) => {
      if (row.key === "local" && attached) return false;
      return available.get(row.key)?.available !== false;
    });
  }, [sources, attached]);
  const rows = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return offered;
    return offered.filter(
      (row) =>
        row.label.toLowerCase().includes(needle) ||
        (row.description ?? "").toLowerCase().includes(needle),
    );
  }, [query, offered]);
  // What the machine says would make a hidden source usable. Shown rather
  // than swallowed: "GitHub is missing" with no reason is worse than absent.
  const unavailable = useMemo(
    () =>
      (sources?.sources ?? []).filter(
        (source) =>
          !source.available &&
          source.remediation &&
          SOURCES.some((row) => row.key === source.kind),
      ),
    [sources],
  );
  // What the machine says about GitHub while still offering it: no `gh`
  // credential means public repositories only. Read from the probe rather
  // than from `getCodeCloneDefaults`, which is administrator-only — a member
  // on a shared machine would otherwise be told nothing at all.
  const githubHint = useMemo(() => {
    const github = sources?.sources.find((source) => source.kind === "github");
    if (github?.available && github.remediation) return github.remediation;
    return null;
  }, [sources]);

  const setCurrentJob = useCallback((nextJobId: string | null) => {
    activeJobIdRef.current = nextJobId;
    setJobId(nextJobId);
  }, []);

  const editParentDir = useCallback((value: string) => {
    destinationEditVersion.current += 1;
    setParentDir(value);
  }, []);

  const isCurrentClient = useCallback(
    (opening: PaletteOpening | null): opening is PaletteOpening =>
      Boolean(
        opening &&
          opening.clientGeneration === codeClientGeneration(client) &&
          opening.clientGeneration ===
            useCodeUpdatesStore.getState().cloneClientGeneration,
      ),
    [client],
  );

  const isCurrentOpening = useCallback(
    (opening: PaletteOpening | null): opening is PaletteOpening =>
      Boolean(
        opening?.active &&
          openingRef.current === opening &&
          isCurrentClient(opening),
      ),
    [isCurrentClient],
  );

  const backgroundCurrentClone = useCallback(() => {
    const handoff = handoffRef.current;
    if (!handoff) return;
    handoff.armed = false;
    setCodeCloneBackground(client, handoff.jobId, true);
  }, [client]);

  const handleOpenChange = useCallback(
    (nextOpen: boolean) => {
      if (!nextOpen) {
        const opening = openingRef.current;
        if (opening) {
          opening.active = false;
          opening.controller.abort();
        }
        backgroundCurrentClone();
      }
      onOpenChange(nextOpen);
    },
    [backgroundCurrentClone, onOpenChange],
  );

  const loadSources = useCallback(
    async (opening: PaletteOpening, showBusy = false) => {
      if (showBusy) setProbeBusy("sources");
      try {
        const next = await client.getCodeRepoSources(opening.controller.signal);
        if (!isCurrentOpening(opening)) return;
        setSources(next);
        setSourcesProbeFailed(false);
      } catch (caught) {
        if (opening.controller.signal.aborted || isAbortFailure(caught)) return;
        if (!isCurrentOpening(opening)) return;
        setSources(null);
        setSourcesProbeFailed(true);
      } finally {
        if (showBusy && isCurrentOpening(opening)) setProbeBusy(null);
      }
    },
    [client, isCurrentOpening],
  );

  const loadGithubRepositories = useCallback(
    async (opening: PaletteOpening, showBusy = false) => {
      if (showBusy) setProbeBusy("github");
      try {
        const next = await client.listCodeGithubRepositories(
          opening.controller.signal,
        );
        if (!isCurrentOpening(opening)) return;
        setGithubRepos(next.repositories);
        setGithubListFailed(false);
      } catch (caught) {
        if (opening.controller.signal.aborted || isAbortFailure(caught)) return;
        if (!isCurrentOpening(opening)) return;
        setGithubRepos([]);
        setGithubListFailed(true);
      } finally {
        if (showBusy && isCurrentOpening(opening)) setProbeBusy(null);
      }
    },
    [client, isCurrentOpening],
  );

  const loadCloneDefaults = useCallback(
    async (
      opening: PaletteOpening,
      options: {
        seedDestination: boolean;
        replaceDestination?: boolean;
        showBusy?: boolean;
      },
    ) => {
      const {
        seedDestination,
        replaceDestination = false,
        showBusy = false,
      } = options;
      const editVersion = destinationEditVersion.current;
      const maySeedDestination =
        seedDestination &&
        (replaceDestination || destinationEditVersion.current === 0);
      if (showBusy) setProbeBusy("defaults");
      try {
        const next = await client.getCodeCloneDefaults(
          opening.controller.signal,
        );
        if (!isCurrentOpening(opening)) return;
        setLoadedDefaults({
          clientGeneration: opening.clientGeneration,
          value: next,
        });
        setDefaultsProbeFailed(false);
        if (
          maySeedDestination &&
          destinationEditVersion.current === editVersion
        ) {
          setParentDir(next.parent_dir ?? "");
        }
      } catch (caught) {
        if (opening.controller.signal.aborted || isAbortFailure(caught)) return;
        if (!isCurrentOpening(opening)) return;
        setLoadedDefaults(null);
        setDefaultsProbeFailed(!isAuthorizationFailure(caught));
      } finally {
        if (showBusy && isCurrentOpening(opening)) setProbeBusy(null);
      }
    },
    [client, isCurrentOpening],
  );

  const showClone = useCallback(
    (
      opening: PaletteOpening,
      resumable: ResumableCodeClone,
      allowAutomaticHandoff: boolean,
    ) => {
      const { request } = resumable.tracking;
      setUrl(request.url ?? "");
      setGithub(request.github ?? "");
      destinationEditVersion.current += 1;
      setParentDir(request.parent_dir ?? "");
      setCloneName(request.name ?? "");
      setCurrentJob(resumable.job.id);
      setStage("progress");
      setCodeCloneBackground(client, resumable.job.id, false);
      handoffRef.current = {
        openingId: opening.id,
        clientGeneration: opening.clientGeneration,
        jobId: resumable.job.id,
        armed:
          allowAutomaticHandoff &&
          !resumable.tracking.background &&
          !resumable.job.done,
      };
    },
    [client, setCurrentJob],
  );

  useEffect(() => {
    const clientGeneration = activateCodeCloneClient(client);
    if (!open) {
      openingRef.current = null;
      return;
    }
    openingSequence.current += 1;
    const opening: PaletteOpening = {
      id: openingSequence.current,
      clientGeneration,
      active: true,
      controller: new AbortController(),
    };
    openingRef.current = opening;
    handoffRef.current = null;
    activeJobIdRef.current = null;
    autoAttemptedJobs.current.clear();
    setStage("sources");
    setQuery("");
    setPath("");
    setDisplayName("");
    setUrl("");
    setGithub("");
    setParentDir("");
    destinationEditVersion.current = 0;
    setCloneName("");
    setCurrentJob(null);
    setBusy(false);
    setError(null);
    setHandoffBusy(false);
    setHandoffError(null);
    setLoadedDefaults(null);
    setSources(null);
    setSourcesProbeFailed(false);
    setDefaultsProbeFailed(false);
    setGithubRepos(null);
    setGithubListFailed(false);
    setProbeBusy(null);

    const selected = takeSelectedCodeClone(client);
    const resumable =
      selected === undefined ? latestCodeClone(client) : selected;
    if (resumable) {
      showClone(opening, resumable, selected === undefined);
    }

    void loadSources(opening);
    void loadGithubRepositories(opening);
    // Administrator-only, and the person adding a repo on a shared machine
    // usually is not one. A refusal leaves the remembered destination unknown,
    // which the machine fills in for itself.
    void loadCloneDefaults(opening, {
      seedDestination: !resumable,
    });

    return () => {
      opening.active = false;
      opening.controller.abort();
      if (openingRef.current === opening) openingRef.current = null;
      const handoff = handoffRef.current;
      if (
        handoff?.openingId === opening.id &&
        handoff.clientGeneration === clientGeneration
      ) {
        handoff.armed = false;
        setCodeCloneBackground(client, handoff.jobId, true);
      }
    };
  }, [
    client,
    loadCloneDefaults,
    loadGithubRepositories,
    loadSources,
    open,
    setCurrentJob,
    showClone,
  ]);

  useEffect(() => {
    if (!open || !selectedClone) return;
    const opening = openingRef.current;
    if (!isCurrentOpening(opening)) return;
    const resumable = takeSelectedCodeClone(client);
    if (!resumable) return;
    if (activeJobIdRef.current !== resumable.job.id) backgroundCurrentClone();
    handoffRef.current = null;
    showClone(opening, resumable, false);
  }, [
    backgroundCurrentClone,
    client,
    isCurrentOpening,
    open,
    selectedClone,
    showClone,
  ]);

  useEffect(() => {
    if (!open || stage !== "progress" || !jobId || job?.done || readError) {
      return;
    }
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const poll = async () => {
      const next = await reconcileCodeClone(client, jobId);
      if (cancelled || !next || next.done) return;
      timer = setTimeout(() => void poll(), CLONE_POLL_INTERVAL_MS);
    };
    void poll();
    return () => {
      cancelled = true;
      if (timer !== null) clearTimeout(timer);
    };
  }, [client, job?.done, jobId, open, readError, stage]);

  const completeClone = useCallback(
    async (requireAutomaticHandoff: boolean) => {
      const opening = openingRef.current;
      const currentJob = activeJobIdRef.current
        ? useCodeUpdatesStore.getState().cloneJobs[activeJobIdRef.current]
        : undefined;
      if (
        !isCurrentOpening(opening) ||
        !currentJob?.done ||
        currentJob.error ||
        !currentJob.repo_id
      ) {
        return;
      }
      const handoff = handoffRef.current;
      if (
        requireAutomaticHandoff &&
        (!handoff?.armed ||
          handoff.jobId !== currentJob.id ||
          handoff.openingId !== opening.id ||
          handoff.clientGeneration !== opening.clientGeneration)
      ) {
        return;
      }

      setHandoffBusy(true);
      setHandoffError(null);
      try {
        const repo = await client.getCodeRepo(currentJob.repo_id);
        if (!isCurrentOpening(opening)) return;
        if (requireAutomaticHandoff && !handoffRef.current?.armed) return;
        upsertRepo(repo);
        forgetCodeClone(client, currentJob.id);
        handoffRef.current = null;
        opening.active = false;
        onOpenChange(false);
        // A registered repo does nothing on its own. Hand the reader straight
        // to the one thing it is for, with the new repo picked.
        useCodeUiStore.getState().startNewWorkspace(repo.id);
      } catch (caught) {
        if (isCurrentOpening(opening)) {
          setHandoffError(
            friendlyErrorMessage(
              caught,
              "Cloned, but could not open the repository",
            ),
          );
        }
      } finally {
        if (isCurrentOpening(opening)) setHandoffBusy(false);
      }
    },
    [client, isCurrentOpening, onOpenChange, upsertRepo],
  );

  useEffect(() => {
    if (
      !open ||
      stage !== "progress" ||
      !job?.done ||
      job.error ||
      !job.repo_id ||
      autoAttemptedJobs.current.has(job.id)
    ) {
      return;
    }
    const handoff = handoffRef.current;
    if (!handoff?.armed || handoff.jobId !== job.id) return;
    autoAttemptedJobs.current.add(job.id);
    void completeClone(true);
  }, [completeClone, job, open, stage]);

  function goBack() {
    if (stage === "sources") {
      handleOpenChange(false);
      return;
    }
    if (stage === "progress") {
      const currentJob = activeJobIdRef.current
        ? useCodeUpdatesStore.getState().cloneJobs[activeJobIdRef.current]
        : undefined;
      if (currentJob?.done) forgetCodeClone(client, currentJob.id);
      else backgroundCurrentClone();
      handoffRef.current = null;
      setStage(github.trim() ? "github" : url.trim() ? "git_url" : "sources");
      setCurrentJob(null);
      setError(null);
      setHandoffError(null);
      return;
    }
    setStage("sources");
    setError(null);
  }

  async function pickDirectory(into: "path" | "parent") {
    const opening = openingRef.current;
    const picked = await pickCodeDirectory();
    if (!picked || !isCurrentOpening(opening)) return;
    if (into === "path") setPath(picked);
    else editParentDir(picked);
  }

  async function registerLocal() {
    if (!path.trim()) return;
    const opening = openingRef.current;
    if (!isCurrentOpening(opening)) return;
    setBusy(true);
    setError(null);
    try {
      const repo = await client.createCodeRepo({
        path: path.trim(),
        display_name: displayName.trim() || undefined,
      });
      if (!isCurrentClient(opening)) return;
      upsertRepo(repo);
      if (isCurrentOpening(opening)) {
        opening.active = false;
        onOpenChange(false);
        useCodeUiStore.getState().startNewWorkspace(repo.id);
      } else {
        toast.success("Repository registered", {
          description: "Create a workspace when you are ready.",
          action: {
            label: "Open",
            onClick: () => useCodeUiStore.getState().startNewWorkspace(repo.id),
          },
        });
      }
    } catch (caught) {
      if (isCurrentOpening(opening)) {
        setError(
          friendlyErrorMessage(caught, "Could not register that repository"),
        );
      }
    } finally {
      if (isCurrentOpening(opening)) setBusy(false);
    }
  }

  async function startClone(body: {
    url?: string;
    github?: string;
    parent_dir?: string;
    name?: string;
  }) {
    // A field nobody was shown must not be sent. `parentDir` may still hold
    // whatever the defaults read seeded it with, and sending that would put
    // the checkout somewhere the reader never chose.
    const parent = machineChoosesDestination
      ? undefined
      : body.parent_dir?.trim();
    if (!parent && !machineChoosesDestination) return;
    const opening = openingRef.current;
    if (!isCurrentOpening(opening)) return;
    const request: CodeCloneRequest = {
      ...body,
      parent_dir: parent || undefined,
      name: body.name?.trim() || undefined,
    };
    const replacedJobId = activeJobIdRef.current;
    setBusy(true);
    setError(null);
    try {
      const started = await client.startCodeClone(request);
      const current = isCurrentOpening(opening);
      const tracked = trackCodeClone(client, started, request, !current);
      if (!tracked || !current) return;
      if (replacedJobId && replacedJobId !== started.id) {
        forgetCodeClone(client, replacedJobId);
      }
      setCurrentJob(started.id);
      handoffRef.current = {
        openingId: opening.id,
        clientGeneration: opening.clientGeneration,
        jobId: started.id,
        armed: true,
      };
      setStage("progress");
    } catch (caught) {
      if (isCurrentOpening(opening)) {
        setError(friendlyErrorMessage(caught, "Could not start the clone"));
      }
    } finally {
      if (isCurrentOpening(opening)) setBusy(false);
    }
  }

  function selectSource(key: string) {
    const row =
      rows.find((entry) => entry.key === key) ??
      SOURCES.find((entry) => entry.key === key);
    if (!row) return;
    if (row.key === "local") {
      setStage("local");
      if (canBrowse) void pickDirectory("path");
      return;
    }
    if (row.key === "git_url") setStage("git_url");
    if (row.key === "github") setStage("github");
  }

  function submitForm() {
    if (stage === "local") {
      void registerLocal();
      return;
    }
    if (stage === "git_url") {
      void startClone({
        url,
        parent_dir: parentDir,
        name: cloneName,
      });
      return;
    }
    if (stage === "github") {
      void startClone({
        github,
        parent_dir: parentDir,
        name: cloneName,
      });
    }
  }

  function onKeyDown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      handleOpenChange(false);
      return;
    }
    if (
      event.key === "Enter" &&
      (event.metaKey || event.ctrlKey) &&
      !event.altKey &&
      !event.shiftKey
    ) {
      event.preventDefault();
      submitForm();
      return;
    }
    if (event.key === "Backspace") {
      const target = event.target as HTMLElement | null;
      const typing =
        target instanceof HTMLInputElement ||
        target instanceof HTMLTextAreaElement;
      if (typing && target.value.length > 0) return;
      event.preventDefault();
      goBack();
    }
  }

  const cloneSucceeded = job?.done === true && !job.error;
  const cloneFailed = job?.done === true && Boolean(job.error);
  const title =
    stage === "local"
      ? "Local folder"
      : stage === "git_url"
        ? "Git URL"
        : stage === "github"
          ? "GitHub repository"
          : stage === "progress"
            ? cloneSucceeded
              ? "Repository cloned"
              : cloneFailed
                ? "Clone failed"
                : "Cloning"
            : "Add a repo";

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent
        className="max-w-md gap-4 p-5"
        onKeyDown={onKeyDown}
        aria-busy={busy || handoffBusy}
      >
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>
            {stage === "sources"
              ? "Register a local checkout or clone a remote."
              : stage === "local"
                ? "Pick a folder or paste its path."
                : stage === "git_url"
                  ? "Clone from a remote URL into a parent folder."
                  : stage === "github"
                    ? "Clone an owner/repo from GitHub."
                    : cloneSucceeded
                      ? "Create a workspace when you are ready."
                      : cloneFailed
                        ? "Fix the source or destination, then try again."
                        : "You can close this window while the clone continues."}
          </DialogDescription>
        </DialogHeader>

        {stage === "sources" && (
          <Command
            shouldFilter={false}
            label="Sources"
            className="rounded-md border bg-transparent"
          >
            <CommandInput
              value={query}
              onValueChange={setQuery}
              placeholder="Filter sources"
              aria-label="Filter sources"
              className="h-9"
            />
            <CommandList className="max-h-56">
              <CommandEmpty className="px-3 py-2 text-xs text-muted-foreground">
                Nothing matches.
              </CommandEmpty>
              <CommandGroup className="p-1">
                {rows.map((row) => {
                  const Icon = row.icon;
                  return (
                    <CommandItem
                      key={row.key}
                      value={row.key}
                      onSelect={() => selectSource(row.key)}
                    >
                      <Icon
                        className="size-4 shrink-0 text-muted-foreground"
                        aria-hidden="true"
                      />
                      <span className="flex min-w-0 flex-col">
                        <span className="truncate">{row.label}</span>
                        {row.description && (
                          <span className="truncate text-xs text-muted-foreground">
                            {row.description}
                          </span>
                        )}
                      </span>
                    </CommandItem>
                  );
                })}
              </CommandGroup>
              {unavailable.length > 0 && (
                <div className="border-t px-3 py-2">
                  {unavailable.map((source) => (
                    <p
                      key={source.kind}
                      className="text-xs text-muted-foreground"
                    >
                      {source.remediation}
                    </p>
                  ))}
                </div>
              )}
            </CommandList>
          </Command>
        )}

        {stage === "sources" && sourcesProbeFailed && (
          <ProbeFailure
            message="Could not check which sources this machine supports."
            busy={probeBusy === "sources"}
            onRetry={() => {
              const opening = openingRef.current;
              if (isCurrentOpening(opening)) void loadSources(opening, true);
            }}
          />
        )}

        {stage === "local" && (
          <LocalStage
            path={path}
            displayName={displayName}
            busy={busy}
            error={error}
            onPath={setPath}
            onDisplayName={setDisplayName}
            canBrowse={canBrowse}
            onBrowse={() => void pickDirectory("path")}
            onSubmit={() => void registerLocal()}
            command={command}
          />
        )}

        {stage === "git_url" && (
          <GitUrlStage
            url={url}
            parentDir={parentDir}
            name={cloneName}
            busy={busy}
            error={error}
            onUrl={setUrl}
            onParentDir={editParentDir}
            onName={setCloneName}
            canBrowse={canBrowse}
            attached={attached}
            machineChoosesDestination={machineChoosesDestination}
            defaultsProbeFailed={defaultsProbeFailed}
            defaultsBusy={probeBusy === "defaults"}
            onBrowse={() => void pickDirectory("parent")}
            onRetryDefaults={() => {
              const opening = openingRef.current;
              if (isCurrentOpening(opening)) {
                void loadCloneDefaults(opening, {
                  seedDestination: true,
                  replaceDestination: true,
                  showBusy: true,
                });
              }
            }}
            onSubmit={() =>
              void startClone({
                url,
                parent_dir: parentDir,
                name: cloneName,
              })
            }
            command={command}
          />
        )}

        {stage === "github" && (
          <GithubStage
            github={github}
            parentDir={parentDir}
            name={cloneName}
            defaults={defaults}
            hint={githubHint}
            repositories={githubRepos}
            listFailed={githubListFailed}
            listBusy={probeBusy === "github"}
            busy={busy}
            error={error}
            onGithub={setGithub}
            onParentDir={editParentDir}
            onName={setCloneName}
            canBrowse={canBrowse}
            attached={attached}
            machineChoosesDestination={machineChoosesDestination}
            defaultsProbeFailed={defaultsProbeFailed}
            defaultsBusy={probeBusy === "defaults"}
            onBrowse={() => void pickDirectory("parent")}
            onRetryRepositories={() => {
              const opening = openingRef.current;
              if (isCurrentOpening(opening)) {
                void loadGithubRepositories(opening, true);
              }
            }}
            onRetryDefaults={() => {
              const opening = openingRef.current;
              if (isCurrentOpening(opening)) {
                void loadCloneDefaults(opening, {
                  seedDestination: true,
                  replaceDestination: true,
                  showBusy: true,
                });
              }
            }}
            onSubmit={() =>
              void startClone({
                github,
                parent_dir: parentDir,
                name: cloneName,
              })
            }
            command={command}
          />
        )}

        {stage === "progress" && (
          <ProgressStage
            job={job}
            readError={readError}
            handoffError={handoffError}
            handoffBusy={handoffBusy}
            onRetry={() => {
              if (jobId) forgetCodeClone(client, jobId);
              setStage(github.trim() ? "github" : "git_url");
              setCurrentJob(null);
              handoffRef.current = null;
              setError(null);
              setHandoffError(null);
            }}
            onResume={() => {
              if (jobId) void reconcileCodeClone(client, jobId);
            }}
            onCreateWorkspace={() => void completeClone(false)}
          />
        )}

        <footer className="text-muted-foreground flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
          {stage === "sources" && (
            <>
              <Hint keys={["↑", "↓"]} label="Navigate" />
              <Hint keys={["Enter"]} label="Select" />
            </>
          )}
          <Hint keys={["Backspace"]} label="Back" />
          <Hint keys={["Esc"]} label="Close" />
        </footer>
      </DialogContent>
    </Dialog>
  );
}

function LocalStage({
  path,
  displayName,
  busy,
  error,
  onPath,
  onDisplayName,
  canBrowse,
  onBrowse,
  onSubmit,
  command,
}: {
  path: string;
  displayName: string;
  busy: boolean;
  error: string | null;
  onPath: (value: string) => void;
  onDisplayName: (value: string) => void;
  /** Whether this window can open a picker the machine would resolve. */
  canBrowse: boolean;
  onBrowse: () => void;
  onSubmit: () => void;
  command: boolean;
}) {
  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(event) => {
        event.preventDefault();
        onSubmit();
      }}
    >
      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">Path</span>
        <div className="flex gap-2">
          <Input
            value={path}
            onChange={(event) => onPath(event.target.value)}
            placeholder={
              canBrowse ? "/Users/you/src/app" : "a path on the machine"
            }
            disabled={busy}
            autoFocus
          />
          {canBrowse && (
            <Button
              type="button"
              variant="outline"
              onClick={onBrowse}
              disabled={busy}
            >
              Browse
            </Button>
          )}
        </div>
      </label>
      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">Display name</span>
        <Input
          value={displayName}
          onChange={(event) => onDisplayName(event.target.value)}
          disabled={busy}
        />
      </label>
      {error && <p className="text-sm text-critical">{error}</p>}
      <FormSubmit
        busy={busy}
        disabled={!path.trim()}
        busyLabel="Registering…"
        label="Register"
        command={command}
      />
    </form>
  );
}

function GitUrlStage({
  url,
  parentDir,
  name,
  busy,
  error,
  onUrl,
  onParentDir,
  onName,
  canBrowse,
  attached,
  machineChoosesDestination,
  defaultsProbeFailed,
  defaultsBusy,
  onBrowse,
  onRetryDefaults,
  onSubmit,
  command,
}: {
  url: string;
  parentDir: string;
  name: string;
  busy: boolean;
  error: string | null;
  onUrl: (value: string) => void;
  onParentDir: (value: string) => void;
  onName: (value: string) => void;
  canBrowse: boolean;
  attached: boolean;
  machineChoosesDestination: boolean;
  defaultsProbeFailed: boolean;
  defaultsBusy: boolean;
  onBrowse: () => void;
  onRetryDefaults: () => void;
  onSubmit: () => void;
  command: boolean;
}) {
  const destinationBlocked = attached && !machineChoosesDestination;
  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(event) => {
        event.preventDefault();
        onSubmit();
      }}
    >
      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">URL</span>
        <Input
          value={url}
          onChange={(event) => onUrl(event.target.value)}
          placeholder="https://example.com/acme/app.git"
          disabled={busy}
          autoFocus
        />
      </label>
      {!machineChoosesDestination && (
        <ParentDirField
          value={parentDir}
          busy={busy}
          canBrowse={canBrowse}
          blocked={destinationBlocked}
          defaultsProbeFailed={defaultsProbeFailed}
          defaultsBusy={defaultsBusy}
          onChange={onParentDir}
          onBrowse={onBrowse}
          onRetryDefaults={onRetryDefaults}
        />
      )}
      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">Name</span>
        <Input
          value={name}
          onChange={(event) => onName(event.target.value)}
          placeholder="optional folder name"
          disabled={busy}
        />
      </label>
      {error && <p className="text-sm text-critical">{error}</p>}
      <FormSubmit
        busy={busy}
        disabled={
          !url.trim() ||
          destinationBlocked ||
          (!machineChoosesDestination && !parentDir.trim())
        }
        busyLabel="Starting…"
        label="Clone"
        command={command}
      />
    </form>
  );
}

function GithubStage({
  github,
  parentDir,
  name,
  defaults,
  hint,
  repositories,
  listFailed,
  listBusy,
  busy,
  error,
  onGithub,
  onParentDir,
  onName,
  canBrowse,
  attached,
  machineChoosesDestination,
  defaultsProbeFailed,
  defaultsBusy,
  onBrowse,
  onRetryRepositories,
  onRetryDefaults,
  onSubmit,
  command,
}: {
  github: string;
  parentDir: string;
  name: string;
  defaults: CodeCloneDefaults | null;
  /** The machine's note about GitHub, when it still offers it. */
  hint: string | null;
  repositories: CodeGithubRepository[] | null;
  listFailed: boolean;
  listBusy: boolean;
  busy: boolean;
  error: string | null;
  onGithub: (value: string) => void;
  onParentDir: (value: string) => void;
  onName: (value: string) => void;
  canBrowse: boolean;
  attached: boolean;
  machineChoosesDestination: boolean;
  defaultsProbeFailed: boolean;
  defaultsBusy: boolean;
  onBrowse: () => void;
  onRetryRepositories: () => void;
  onRetryDefaults: () => void;
  onSubmit: () => void;
  command: boolean;
}) {
  // The machine's own note first; the administrator-only defaults read is
  // the fallback for a profile whose probe predates this field.
  const ghHint =
    hint ??
    (defaults && (!defaults.gh_found || defaults.gh_authenticated === false)
      ? defaults.gh_remediation
      : null);
  const destinationBlocked = attached && !machineChoosesDestination;
  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(event) => {
        event.preventDefault();
        onSubmit();
      }}
    >
      <GithubRepoField
        value={github}
        repositories={repositories}
        listFailed={listFailed}
        listBusy={listBusy}
        busy={busy}
        onChange={onGithub}
        onRetry={onRetryRepositories}
      />
      {ghHint && (
        <p
          className="text-muted-foreground text-xs"
          data-testid="gh-absent-hint"
        >
          {ghHint}
          {!ghHint.startsWith("Clones and pushes") &&
            " You can still clone over HTTPS."}
        </p>
      )}
      {!machineChoosesDestination && (
        <ParentDirField
          value={parentDir}
          busy={busy}
          canBrowse={canBrowse}
          blocked={destinationBlocked}
          defaultsProbeFailed={defaultsProbeFailed}
          defaultsBusy={defaultsBusy}
          onChange={onParentDir}
          onBrowse={onBrowse}
          onRetryDefaults={onRetryDefaults}
        />
      )}
      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">Name</span>
        <Input
          value={name}
          onChange={(event) => onName(event.target.value)}
          placeholder="optional folder name"
          disabled={busy}
        />
      </label>
      {error && <p className="text-sm text-critical">{error}</p>}
      <FormSubmit
        busy={busy}
        disabled={
          !github.trim() ||
          destinationBlocked ||
          (!machineChoosesDestination && !parentDir.trim())
        }
        busyLabel="Starting…"
        label="Clone"
        command={command}
      />
    </form>
  );
}

function GithubRepoField({
  value,
  repositories,
  listFailed,
  listBusy,
  busy,
  onChange,
  onRetry,
}: {
  value: string;
  repositories: CodeGithubRepository[] | null;
  listFailed: boolean;
  listBusy: boolean;
  busy: boolean;
  onChange: (value: string) => void;
  onRetry: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const listed = repositories ?? [];
  const emptySuccess =
    !listFailed && listed.length === 0 && repositories !== null;
  const needle = query.trim().toLowerCase();
  const matches = listed
    .filter((repository) => {
      if (!needle) return true;
      return (
        repository.full_name.toLowerCase().includes(needle) ||
        (repository.description ?? "").toLowerCase().includes(needle)
      );
    })
    .sort(compareGithubRepositories);
  const typed = query.trim();
  const typedIsNew =
    typed.includes("/") &&
    !listed.some(
      (repository) =>
        repository.full_name.toLowerCase() === typed.toLowerCase(),
    );

  return (
    <label className="flex flex-col gap-1 text-sm">
      <span className="font-medium">Repository</span>
      {emptySuccess ? (
        <Input
          value={value}
          onChange={(event) => onChange(event.target.value)}
          placeholder="owner/repo"
          disabled={busy}
          autoFocus
        />
      ) : (
        <Popover
          modal
          open={open}
          onOpenChange={(next) => {
            setOpen(next);
            if (next) setQuery(value);
          }}
        >
          <PopoverTrigger asChild>
            <Button
              type="button"
              variant="outline"
              role="combobox"
              aria-expanded={open}
              aria-label="Repository"
              disabled={busy}
              className={cn(
                "h-9 w-full justify-between font-normal",
                !value && "text-muted-foreground",
              )}
            >
              <span className="flex min-w-0 items-center gap-2">
                {value && <GithubAvatar login={githubRepoOwner(value)} />}
                <span className="truncate">
                  {value || "Select a repository"}
                </span>
              </span>
              <ChevronDown className="size-4 shrink-0 opacity-50" />
            </Button>
          </PopoverTrigger>
          <PopoverContent
            align="start"
            collisionPadding={12}
            className="w-[var(--radix-popover-trigger-width)] overflow-hidden p-0"
          >
            <Command
              shouldFilter={false}
              label="Repositories"
              className="h-auto"
            >
              <CommandInput
                value={query}
                onValueChange={setQuery}
                placeholder="Search or type owner/repo"
                aria-label="Search repositories"
              />
              <CommandList
                className="max-h-64 overflow-y-auto overscroll-contain"
                style={{
                  maxHeight:
                    "min(16rem, calc(var(--radix-popover-content-available-height, 100vh) - 3.25rem))",
                }}
              >
                <CommandEmpty className="px-3 py-2 text-xs text-muted-foreground">
                  {listFailed
                    ? "Suggestions did not load. Type owner/repo to clone."
                    : repositories === null
                      ? "Loading repositories…"
                      : "Nothing matches. Type owner/repo to clone it."}
                </CommandEmpty>
                {typedIsNew && (
                  <CommandGroup heading="Use this name">
                    <CommandItem
                      value={typed}
                      onSelect={() => {
                        onChange(typed);
                        setOpen(false);
                      }}
                    >
                      <GithubAvatar login={githubRepoOwner(typed)} />
                      <span className="truncate">{typed}</span>
                    </CommandItem>
                  </CommandGroup>
                )}
                {matches.length > 0 && (
                  <CommandGroup heading="Your repositories">
                    {matches.slice(0, 50).map((repository) => (
                      <CommandItem
                        key={repository.full_name}
                        value={repository.full_name}
                        onSelect={() => {
                          onChange(repository.full_name);
                          setOpen(false);
                        }}
                      >
                        <GithubAvatar
                          login={githubRepoOwner(repository.full_name)}
                        />
                        <span className="flex min-w-0 flex-col">
                          <span className="truncate">
                            {repository.full_name}
                          </span>
                          {repository.description && (
                            <span className="text-muted-foreground truncate text-xs">
                              {repository.description}
                            </span>
                          )}
                        </span>
                      </CommandItem>
                    ))}
                  </CommandGroup>
                )}
              </CommandList>
            </Command>
          </PopoverContent>
        </Popover>
      )}
      {listFailed && (
        <div className="flex items-center justify-between gap-3 text-xs">
          <p className="text-muted-foreground" data-testid="github-list-failed">
            Suggestions did not load. Type owner/repo or retry.
          </p>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="shrink-0"
            disabled={busy || listBusy}
            onClick={onRetry}
          >
            {listBusy ? "Retrying…" : "Retry"}
          </Button>
        </div>
      )}
    </label>
  );
}

function FormSubmit({
  busy,
  disabled,
  busyLabel,
  label,
  command,
}: {
  busy: boolean;
  disabled: boolean;
  busyLabel: string;
  label: string;
  command: boolean;
}) {
  return (
    <DialogFooter>
      <Button type="submit" disabled={busy || disabled}>
        {busy ? busyLabel : label}
        {!busy && (
          <span
            className="ml-1 inline-flex items-center gap-0.5 text-2xs font-medium opacity-60"
            aria-hidden="true"
          >
            <kbd className="font-sans">{command ? "⌘" : "Ctrl"}</kbd>
            <kbd className="font-sans">↩</kbd>
          </span>
        )}
      </Button>
    </DialogFooter>
  );
}

function ParentDirField({
  value,
  busy,
  canBrowse,
  blocked,
  defaultsProbeFailed,
  defaultsBusy,
  onChange,
  onBrowse,
  onRetryDefaults,
}: {
  value: string;
  busy: boolean;
  canBrowse: boolean;
  blocked: boolean;
  defaultsProbeFailed: boolean;
  defaultsBusy: boolean;
  onChange: (value: string) => void;
  onBrowse: () => void;
  onRetryDefaults: () => void;
}) {
  if (blocked) {
    return (
      <p
        className="text-sm text-critical"
        data-testid="clone-destination-missing"
      >
        This machine has no clone destination configured. An administrator sets
        one on the machine.
      </p>
    );
  }
  return (
    <label className="flex flex-col gap-1 text-sm">
      <span className="font-medium">Destination folder</span>
      <div className="flex gap-2">
        <Input
          value={value}
          onChange={(event) => onChange(event.target.value)}
          placeholder={canBrowse ? "/Users/you/src" : "a path on the machine"}
          disabled={busy}
        />
        {canBrowse && (
          <Button
            type="button"
            variant="outline"
            onClick={onBrowse}
            disabled={busy}
          >
            Browse
          </Button>
        )}
      </div>
      {defaultsProbeFailed && (
        <div className="flex items-center justify-between gap-3 text-xs">
          <p className="text-muted-foreground">
            The saved destination did not load. Choose one or retry.
          </p>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="shrink-0"
            disabled={busy || defaultsBusy}
            onClick={onRetryDefaults}
          >
            {defaultsBusy ? "Retrying…" : "Retry"}
          </Button>
        </div>
      )}
    </label>
  );
}

function ProgressStage({
  job,
  readError,
  handoffError,
  handoffBusy,
  onRetry,
  onResume,
  onCreateWorkspace,
}: {
  job: CodeCloneJobSnapshot | undefined;
  readError: string | undefined;
  handoffError: string | null;
  handoffBusy: boolean;
  onRetry: () => void;
  onResume: () => void;
  onCreateWorkspace: () => void;
}) {
  const failed = job?.done === true && Boolean(job.error);
  const completed = job?.done === true && !job.error && Boolean(job.repo_id);
  const percent = job?.percent ?? (job?.done && !job.error ? 100 : 0);
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="text-sm font-medium" data-testid="clone-phase">
            {job?.phase ?? "Starting"}
          </p>
          {!job?.done && (
            <p className="mt-0.5 text-xs text-muted-foreground">
              This clone keeps running if you close this window.
            </p>
          )}
        </div>
        {job?.percent !== undefined && (
          <span className="shrink-0 font-mono text-xs text-muted-foreground">
            {Math.round(job.percent)}%
          </span>
        )}
      </div>
      <Progress value={percent} style={{ forcedColorAdjust: "none" }} />
      {readError && !job?.done && (
        <ProbeFailure
          message={readError}
          busy={false}
          action="Retry check"
          onRetry={onResume}
        />
      )}
      {failed && (
        <>
          <pre className="bg-muted max-h-32 overflow-auto rounded-md p-2 text-xs whitespace-pre-wrap">
            {job?.error}
          </pre>
          <Button
            type="button"
            variant="outline"
            className="self-start"
            onClick={onRetry}
          >
            Retry
          </Button>
        </>
      )}
      {completed && (
        <div className="rounded-lg border border-success-border bg-success-background p-3">
          <p className={cn("text-sm font-medium", STATUS_TEXT.ready)}>
            The repository is ready.
          </p>
          <p className="mt-1 text-xs text-success-foreground-muted">
            Create a workspace to start working in the new checkout.
          </p>
          {handoffError && (
            <p className="mt-2 text-xs text-critical">{handoffError}</p>
          )}
          <Button
            type="button"
            className="mt-3"
            disabled={handoffBusy}
            onClick={onCreateWorkspace}
          >
            {handoffBusy
              ? "Opening…"
              : handoffError
                ? "Retry"
                : "Create workspace"}
          </Button>
        </div>
      )}
    </div>
  );
}

function ProbeFailure({
  message,
  busy,
  action = "Retry",
  onRetry,
}: {
  message: string;
  busy: boolean;
  action?: string;
  onRetry: () => void;
}) {
  return (
    <div className="rounded-lg border border-warning-border bg-warning-background p-3">
      <p className={cn("text-sm", STATUS_TEXT.warning)}>{message}</p>
      <Button
        type="button"
        variant="outline"
        size="sm"
        className="mt-2"
        disabled={busy}
        onClick={onRetry}
      >
        {busy ? "Retrying…" : action}
      </Button>
    </div>
  );
}

function githubRepoOwner(fullName: string): string | undefined {
  const owner = fullName.split("/")[0]?.trim();
  return owner || undefined;
}

function compareGithubRepositories(
  left: CodeGithubRepository,
  right: CodeGithubRepository,
): number {
  const [leftOwner = "", leftName = ""] = left.full_name
    .toLowerCase()
    .split("/");
  const [rightOwner = "", rightName = ""] = right.full_name
    .toLowerCase()
    .split("/");
  return (
    leftOwner.localeCompare(rightOwner) ||
    leftName.localeCompare(rightName) ||
    left.full_name.localeCompare(right.full_name)
  );
}

function Hint({ keys, label }: { keys: string[]; label: string }) {
  return (
    <span className="inline-flex items-center gap-1">
      {keys.map((key) => (
        <kbd
          key={key}
          className="inline-flex h-5 min-w-5 items-center justify-center rounded border bg-muted/60 px-1 font-sans text-2xs leading-none font-medium text-foreground/80"
        >
          {key}
        </kbd>
      ))}
      <span>{label}</span>
    </span>
  );
}

export type { SourceKey };
export { SOURCES };
