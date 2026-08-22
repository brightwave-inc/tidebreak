import { useEffect, useMemo, useState, type KeyboardEvent } from "react";
import { Folder, GitBranch, Link2 } from "lucide-react";

import type {
  CodeCloneDefaults,
  CodeCloneJobSnapshot,
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
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { hasLocalHostAuthority, pickCodeDirectory } from "@/host";
import { friendlyErrorMessage } from "@/lib/utils";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore } from "./CodeUiStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

type Stage = "sources" | "local" | "git_url" | "github" | "progress";
type SourceKey = "local" | "git_url" | "github";

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
  const [stage, setStage] = useState<Stage>("sources");
  const [query, setQuery] = useState("");
  const [path, setPath] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [url, setUrl] = useState("");
  const [github, setGithub] = useState("");
  const [parentDir, setParentDir] = useState("");
  const [cloneName, setCloneName] = useState("");
  const [defaults, setDefaults] = useState<CodeCloneDefaults | null>(null);
  const [sources, setSources] = useState<CodeRepoSources | null>(null);
  const [jobId, setJobId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const job = jobId ? cloneJobs[jobId] : undefined;
  // Whether this window can name a path the machine would resolve. Browsing
  // is the host's, resolving is the machine's, and they are the same computer
  // only when the window is not attached elsewhere.
  const canBrowse = hasLocalHostAuthority();
  // The machine places clones itself when its operator configured a
  // destination, which is what lets someone who cannot see its filesystem
  // clone at all. Until the probe answers, assume it does not, so a desktop
  // working on its own machine never loses the field it has always had.
  const machineChoosesDestination = sources?.chooses_destination === true;
  const offered = useMemo(() => {
    // Before the probe answers, offer everything: the machine is the
    // authority, and a dialog that showed nothing while asking would read as
    // a broken dialog rather than a pending one.
    if (!sources) return SOURCES;
    const available = new Map(
      sources.sources.map((source) => [source.kind, source] as const),
    );
    return SOURCES.filter((row) => available.get(row.key)?.available !== false);
  }, [sources]);
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

  useEffect(() => {
    if (!open) return;
    setStage("sources");
    setQuery("");
    setPath("");
    setDisplayName("");
    setUrl("");
    setGithub("");
    setCloneName("");
    setJobId(null);
    setBusy(false);
    setError(null);
    setSources(null);
    void client
      .getCodeRepoSources()
      .then(setSources)
      .catch(() => setSources(null));
    // Administrator-only, and the person adding a repo on a shared machine
    // usually is not one. A refusal leaves the remembered destination unknown,
    // which the machine fills in for itself.
    void client
      .getCodeCloneDefaults()
      .then((next) => {
        setDefaults(next);
        setParentDir(next.parent_dir ?? "");
      })
      .catch(() => setDefaults(null));
  }, [open, client]);

  useEffect(() => {
    if (!job || stage !== "progress") return;
    if (job.done && job.repo_id) {
      void (async () => {
        try {
          const repo = await client.getCodeRepo(job.repo_id!);
          upsertRepo(repo);
          onOpenChange(false);
          // A registered repo does nothing on its own. Hand the reader
          // straight to the one thing it is for, with the new repo picked.
          useCodeUiStore.getState().startNewWorkspace(repo.id);
        } catch (err) {
          setError(
            friendlyErrorMessage(err, "Cloned, but could not open the repo"),
          );
        }
      })();
    }
  }, [job, stage, client, upsertRepo, onOpenChange]);

  function goBack() {
    if (stage === "sources") {
      onOpenChange(false);
      return;
    }
    if (stage === "progress") {
      setStage(github.trim() ? "github" : url.trim() ? "git_url" : "sources");
      setJobId(null);
      setError(null);
      return;
    }
    setStage("sources");
    setError(null);
  }

  async function pickDirectory(into: "path" | "parent") {
    const picked = await pickCodeDirectory();
    if (!picked) return;
    if (into === "path") setPath(picked);
    else setParentDir(picked);
  }

  async function registerLocal() {
    if (!path.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const repo = await client.createCodeRepo({
        path: path.trim(),
        display_name: displayName.trim() || undefined,
      });
      upsertRepo(repo);
      onOpenChange(false);
      useCodeUiStore.getState().startNewWorkspace(repo.id);
    } catch (err) {
      setError(friendlyErrorMessage(err, "Could not register that repo"));
    } finally {
      setBusy(false);
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
    setBusy(true);
    setError(null);
    try {
      const started = await client.startCodeClone({
        ...body,
        parent_dir: parent || undefined,
        name: body.name?.trim() || undefined,
      });
      useCodeUpdatesStore.getState().apply({
        type: "clone_progress",
        job: started,
      });
      setJobId(started.id);
      setStage("progress");
    } catch (err) {
      setError(friendlyErrorMessage(err, "Could not start the clone"));
    } finally {
      setBusy(false);
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

  function onKeyDown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      onOpenChange(false);
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

  const title =
    stage === "local"
      ? "Local folder"
      : stage === "git_url"
        ? "Git URL"
        : stage === "github"
          ? "GitHub repository"
          : stage === "progress"
            ? "Cloning"
            : "Add a repo";

  return (
    <Dialog open={open} onOpenChange={busy ? undefined : onOpenChange}>
      <DialogContent
        className="max-w-md gap-4 p-5"
        onKeyDown={onKeyDown}
        aria-busy={busy}
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
                    : "Cloning into the chosen folder."}
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
            onParentDir={setParentDir}
            onName={setCloneName}
            canBrowse={canBrowse}
            machineChoosesDestination={machineChoosesDestination}
            onBrowse={() => void pickDirectory("parent")}
            onSubmit={() =>
              void startClone({
                url,
                parent_dir: parentDir,
                name: cloneName,
              })
            }
          />
        )}

        {stage === "github" && (
          <GithubStage
            github={github}
            parentDir={parentDir}
            name={cloneName}
            defaults={defaults}
            busy={busy}
            error={error}
            onGithub={setGithub}
            onParentDir={setParentDir}
            onName={setCloneName}
            canBrowse={canBrowse}
            machineChoosesDestination={machineChoosesDestination}
            onBrowse={() => void pickDirectory("parent")}
            onSubmit={() =>
              void startClone({
                github,
                parent_dir: parentDir,
                name: cloneName,
              })
            }
          />
        )}

        {stage === "progress" && (
          <ProgressStage
            job={job}
            error={error ?? job?.error}
            onRetry={() => {
              setStage(github.trim() ? "github" : "git_url");
              setJobId(null);
              setError(null);
            }}
          />
        )}

        <footer className="text-muted-foreground flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
          <Hint keys={["↑", "↓"]} label="Navigate" />
          <Hint keys={["Enter"]} label="Select" />
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
      <Button
        type="submit"
        disabled={busy || !path.trim()}
        className="self-start"
      >
        {busy ? "Registering…" : "Register"}
      </Button>
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
  machineChoosesDestination,
  onBrowse,
  onSubmit,
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
  machineChoosesDestination: boolean;
  onBrowse: () => void;
  onSubmit: () => void;
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
          onChange={onParentDir}
          onBrowse={onBrowse}
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
      <Button
        type="submit"
        disabled={
          busy ||
          !url.trim() ||
          (!machineChoosesDestination && !parentDir.trim())
        }
        className="self-start"
      >
        {busy ? "Starting…" : "Clone"}
      </Button>
    </form>
  );
}

function GithubStage({
  github,
  parentDir,
  name,
  defaults,
  busy,
  error,
  onGithub,
  onParentDir,
  onName,
  canBrowse,
  machineChoosesDestination,
  onBrowse,
  onSubmit,
}: {
  github: string;
  parentDir: string;
  name: string;
  defaults: CodeCloneDefaults | null;
  busy: boolean;
  error: string | null;
  onGithub: (value: string) => void;
  onParentDir: (value: string) => void;
  onName: (value: string) => void;
  canBrowse: boolean;
  machineChoosesDestination: boolean;
  onBrowse: () => void;
  onSubmit: () => void;
}) {
  const ghHint =
    defaults && (!defaults.gh_found || defaults.gh_authenticated === false)
      ? defaults.gh_remediation
      : null;
  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(event) => {
        event.preventDefault();
        onSubmit();
      }}
    >
      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">Repository</span>
        <Input
          value={github}
          onChange={(event) => onGithub(event.target.value)}
          placeholder="owner/repo"
          disabled={busy}
          autoFocus
        />
      </label>
      {ghHint && (
        <p
          className="text-muted-foreground text-xs"
          data-testid="gh-absent-hint"
        >
          {ghHint} You can still clone over HTTPS.
        </p>
      )}
      {!machineChoosesDestination && (
        <ParentDirField
          value={parentDir}
          busy={busy}
          canBrowse={canBrowse}
          onChange={onParentDir}
          onBrowse={onBrowse}
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
      <Button
        type="submit"
        disabled={
          busy ||
          !github.trim() ||
          (!machineChoosesDestination && !parentDir.trim())
        }
        className="self-start"
      >
        {busy ? "Starting…" : "Clone"}
      </Button>
    </form>
  );
}

function ParentDirField({
  value,
  busy,
  canBrowse,
  onChange,
  onBrowse,
}: {
  value: string;
  busy: boolean;
  canBrowse: boolean;
  onChange: (value: string) => void;
  onBrowse: () => void;
}) {
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
    </label>
  );
}

function ProgressStage({
  job,
  error,
  onRetry,
}: {
  job: CodeCloneJobSnapshot | undefined;
  error: string | null | undefined;
  onRetry: () => void;
}) {
  const failed = Boolean(error) || (job?.done === true && Boolean(job.error));
  const percent = job?.percent ?? (job?.done && !job.error ? 100 : 0);
  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm" data-testid="clone-phase">
        {job?.phase ?? "starting"}
      </p>
      <Progress value={percent} />
      {failed && (
        <>
          <pre className="bg-muted max-h-32 overflow-auto rounded-md p-2 text-xs whitespace-pre-wrap">
            {error ?? job?.error}
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
    </div>
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
