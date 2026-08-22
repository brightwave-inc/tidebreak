import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";

import type {
  PermissionMode,
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessDoctorEntry,
  HarnessKind,
} from "../api/types";
import { useApp } from "@/AppContext";
import { Button } from "@/components/ui/button";
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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { friendlyErrorMessage } from "@/lib/utils";
import { usesCommandModifier } from "@/ShellShortcuts";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore } from "./CodeUiStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";
import { HarnessModelMenu } from "./CodeComposer";
import { HarnessInstallNote } from "./HarnessInstallNote";
import { HarnessPicker } from "./HarnessPicker";
import {
  autoIsUnsupervised,
  createPermissionModes,
  defaultCreatePermissionMode,
  gatewayCodeModels,
  harnessUnusableReason,
  PERMISSION_MODE_LABELS,
  ALLOW_ALL_NOTE,
  UNSUPERVISED_AUTO_NOTE,
  type CodeModelOption,
} from "./labels";

/**
 * Create a workspace and its first session, then open it.
 *
 * Every field arrives answered, so the dialog is one keystroke deep: Cmd+Enter
 * from anywhere in it creates with what is on screen. Repo, harness, and model
 * open on what this reader used last — the catalog knows, and `lastCreate`
 * covers a fresh window. The starting prompt is optional: filled in, it lands
 * in the workspace composer after create. The title is optional too: left
 * blank, the server generates a two-word name and later replaces it with one
 * derived from the first turn, the same way chats are named.
 *
 * Permission mode defaults to the most autonomous posture the harness honors
 * (decision 0039, amended). Whichever posture that is, the row states it.
 * The harness picker lists every doctor entry. Ready rows are selectable;
 * unusable ones stay visible and dimmed.
 */

/** The repo this reader worked on last: newest workspace, then storage. */
function recentRepoId(
  repos: readonly CodeRepoSnapshot[],
  workspaces: readonly CodeWorkspaceSnapshot[],
  remembered: string | undefined,
): string {
  const known = (id: string | undefined) =>
    id && repos.some((repo) => repo.id === id) ? id : undefined;
  const newest = [...workspaces]
    .sort((a, b) => b.created_at.localeCompare(a.created_at))
    .find((workspace) => known(workspace.repo_id));
  return known(newest?.repo_id) ?? known(remembered) ?? repos[0]?.id ?? "";
}

/** The engine this reader started last, if it can still be started. */
function recentHarness(
  ready: readonly HarnessDoctorEntry[],
  sessions: Record<string, CodeSessionSnapshot>,
  remembered: HarnessKind | undefined,
): HarnessKind | undefined {
  const newest = Object.values(sessions).sort((a, b) =>
    b.created_at.localeCompare(a.created_at),
  )[0];
  for (const kind of [newest?.harness_kind, remembered]) {
    if (kind && ready.some((entry) => entry.kind === kind)) return kind;
  }
  return ready[0]?.kind;
}

export function NewWorkspaceDialog({
  open,
  onOpenChange,
  repos,
  defaultRepoId,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  repos: CodeRepoSnapshot[];
  defaultRepoId?: string;
}) {
  const navigate = useNavigate();
  const { client, models, defaultModelKey } = useApp();
  const doctor = useCodeCatalogStore((state) => state.doctor);
  const sessions = useCodeCatalogStore((state) => state.sessionsByWorkspace);
  const upsertWorkspace = useCodeCatalogStore((state) => state.upsertWorkspace);
  const rememberSession = useCodeCatalogStore((state) => state.rememberSession);
  const ensureHarnessModels = useCodeCatalogStore(
    (state) => state.ensureHarnessModels,
  );
  const reloadDoctor = useCodeCatalogStore((state) => state.reloadDoctor);
  const harnessInstalls = useCodeUpdatesStore((state) => state.harnessInstalls);
  const lastCreate = useCodeUiStore((state) => state.lastCreate);
  const rememberCreate = useCodeUiStore((state) => state.rememberCreate);
  const [repoId, setRepoId] = useState("");
  const [startingPrompt, setStartingPrompt] = useState("");
  const [title, setTitle] = useState("");
  const [baseRef, setBaseRef] = useState("");
  const [pickedHarness, setPickedHarness] = useState<HarnessKind | null>(null);
  const [permissionMode, setPermissionMode] = useState<PermissionMode | null>(
    null,
  );
  const [creating, setCreating] = useState(false);
  const [model, setModel] = useState<string | undefined>();
  const [modelOptions, setModelOptions] = useState<CodeModelOption[]>([]);
  const [modelLoading, setModelLoading] = useState(false);
  const repoTrigger = useRef<HTMLButtonElement>(null);
  const command = useMemo(() => usesCommandModifier(navigator.userAgent), []);

  const allHarnesses = doctor?.harnesses ?? [];
  const readyHarnesses = allHarnesses.filter(
    (entry) => !harnessUnusableReason(entry),
  );
  // The doctor can land after the dialog opens, so the engine is derived
  // rather than seeded: a pick wins, and until there is one the recent
  // engine follows whatever the report says is ready.
  const harness: HarnessKind =
    (pickedHarness && readyHarnesses.some((e) => e.kind === pickedHarness)
      ? pickedHarness
      : undefined) ??
    recentHarness(readyHarnesses, sessions, lastCreate?.harness) ??
    "claude_code";

  useEffect(() => {
    if (!open) return;
    const { workspaces: known } = useCodeCatalogStore.getState();
    const nextRepo =
      defaultRepoId ?? recentRepoId(repos, known, lastCreate?.repoId);
    setRepoId(nextRepo);
    setStartingPrompt("");
    setTitle("");
    setBaseRef(
      repos.find((repo) => repo.id === nextRepo)?.default_base_ref ?? "",
    );
    setPickedHarness(null);
    setPermissionMode(null);
    setModel(undefined);
    setModelOptions([]);
    setModelLoading(false);
    // Reset against the dialog opening, not against catalog refreshes
    // mid-open — a workspace created elsewhere must not move this form.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, defaultRepoId, repos]);

  // A pin this machine has never installed is minutes of npm. Warming it
  // when the dialog opens — and again when the engine changes — moves that
  // off create, where it was a silent stall. The doctor entry is the only
  // trigger: an engine already on disk is never asked for.
  const doctorEntry = allHarnesses.find((item) => item.kind === harness);
  const needsInstall = Boolean(doctorEntry && !doctorEntry.found);
  const install = harnessInstalls[harness];

  useEffect(() => {
    if (!open || !needsInstall) return;
    let cancelled = false;
    void client.startHarnessInstall(harness).then(
      (snapshot) => {
        // The answer is immediate; the phases after it arrive on the updates
        // socket. Applying it here means the note shows even on the profile
        // that never opened one.
        if (!cancelled) {
          useCodeUpdatesStore
            .getState()
            .apply({ type: "harness_install", install: snapshot });
        }
      },
      // Nothing is broken yet: create still reports `harness_not_found` with
      // the reason, and a member of a shared deployment may not install at
      // all. A toast on dialog open would be noise either way.
      () => {},
    );
    return () => {
      cancelled = true;
    };
  }, [client, harness, needsInstall, open]);

  useEffect(() => {
    // A finished install leaves the doctor report saying "Not installed", so
    // the engine would stay unpickable until something re-read it.
    if (!open || !needsInstall || !install?.done || install.error) return;
    void reloadDoctor(client).catch(() => {});
  }, [client, install, needsInstall, open, reloadDoctor]);

  const selectedRepo = repos.find((repo) => repo.id === repoId);
  const selectedHarness = readyHarnesses.find(
    (entry) => entry.kind === harness,
  );
  const availableModes = selectedHarness
    ? createPermissionModes(selectedHarness.caps)
    : [];
  const postedMode =
    permissionMode && availableModes.includes(permissionMode)
      ? permissionMode
      : selectedHarness
        ? defaultCreatePermissionMode(selectedHarness.caps)
        : "plan";
  const canCreate =
    Boolean(repoId && selectedRepo && selectedHarness) && !creating;

  useEffect(() => {
    if (!open || !harness) return;
    // The reader's last model wins where it is still on offer; otherwise the
    // catalog's default, then the first row.
    const pick = (listed: CodeModelOption[]) => (current?: string) => {
      for (const candidate of [current, lastCreate?.model]) {
        if (candidate && listed.some((option) => option.id === candidate)) {
          return candidate;
        }
      }
      return listed.find((option) => option.default)?.id ?? listed[0]?.id;
    };
    const gateway = gatewayCodeModels(models, harness, defaultModelKey);
    if (gateway.length > 0) {
      setModelOptions(gateway);
      setModel(pick(gateway));
      setModelLoading(false);
      return;
    }
    setModelOptions([]);
    setModel(undefined);
    setModelLoading(true);
    let cancelled = false;
    void ensureHarnessModels(client, harness).then((listed) => {
      if (cancelled) return;
      setModelOptions(listed);
      setModel(pick(listed));
      setModelLoading(false);
    });
    return () => {
      cancelled = true;
    };
    // `lastCreate` is a seed, not a subscription: re-running on it would
    // undo a deliberate pick the moment another create records one.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, defaultModelKey, ensureHarnessModels, harness, models, open]);

  async function create() {
    if (!canCreate) return;
    setCreating(true);
    try {
      let workspace: CodeWorkspaceSnapshot;
      try {
        workspace = await client.createCodeWorkspace({
          repo_id: repoId,
          title: title.trim() || undefined,
          base_ref: baseRef.trim() || undefined,
        });
      } catch (error) {
        toast.error(
          friendlyErrorMessage(error, "Could not create the workspace"),
        );
        return;
      }
      upsertWorkspace(workspace);
      const prompt = startingPrompt.trim();
      if (prompt) {
        useCodeUiStore.getState().offerComposerPrompt(workspace.id, prompt);
      }
      try {
        const gateway = gatewayCodeModels(models, harness, defaultModelKey);
        const listed =
          gateway.length > 0
            ? gateway
            : await ensureHarnessModels(client, harness);
        const posted =
          model ?? listed.find((option) => option.default)?.id ?? listed[0]?.id;
        const session = await client.createCodeSession(workspace.id, {
          harness,
          permission_mode: postedMode,
          model: posted,
        });
        rememberSession(session);
        rememberCreate({ repoId, harness, model: posted });
      } catch (error) {
        toast.error(
          `Workspace created, but the session could not start. ${friendlyErrorMessage(error, "Try again from the workspace.")}`,
        );
      }
      onOpenChange(false);
      await navigate({
        to: "/code/w/$workspaceId",
        params: { workspaceId: workspace.id },
      });
    } finally {
      setCreating(false);
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    void create();
  }

  return (
    <Dialog open={open} onOpenChange={creating ? undefined : onOpenChange}>
      <DialogContent
        className="max-w-md gap-5 p-5"
        aria-busy={creating}
        onOpenAutoFocus={(event) => {
          const trigger = repoTrigger.current;
          if (!trigger || trigger.hasAttribute("disabled")) return;
          event.preventDefault();
          trigger.focus();
        }}
        onKeyDownCapture={(event) => {
          if (event.key !== "Enter") return;
          // Cmd+Enter (Ctrl+Enter off macOS) creates with what is on screen,
          // whichever field has focus. An open picker portals its list out of
          // this element, but React still routes the key through here, so the
          // chord works with a dropdown up as well.
          if (event.metaKey || event.ctrlKey) {
            event.preventDefault();
            void create();
            return;
          }
          // Plain Enter stays field-local. A single-line input inside a form
          // submits it, so Enter on Title or Base ref used to cut a worktree
          // from a keystroke that only meant "done typing" — and the create
          // it started left the chord looking broken for the rest of the
          // dialog's life. The check is on this element rather than on each
          // field so a picker's own search box, which lives in a portal, is
          // left to handle its own Enter.
          if (
            event.target instanceof HTMLInputElement &&
            event.currentTarget.contains(event.target)
          ) {
            event.preventDefault();
          }
        }}
      >
        <DialogHeader>
          <DialogTitle>New workspace</DialogTitle>
          <DialogDescription>
            One worktree and one session on the selected repo.
          </DialogDescription>
        </DialogHeader>
        <form className="flex flex-col gap-3" onSubmit={submit}>
          <div className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Repo</span>
            <Select
              value={repoId || undefined}
              onValueChange={(value) => {
                setRepoId(value);
                const next = repos.find((repo) => repo.id === value);
                if (next) setBaseRef(next.default_base_ref);
              }}
              disabled={creating || repos.length === 0}
            >
              <SelectTrigger aria-label="Repo" ref={repoTrigger}>
                <SelectValue placeholder="No repos" />
              </SelectTrigger>
              <SelectContent scrollButtons={false}>
                {repos.map((repo) => (
                  <SelectItem key={repo.id} value={repo.id}>
                    {repo.display_name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Harness</span>
            <HarnessPicker
              harnesses={allHarnesses}
              value={harness}
              onChange={(next) => {
                setModelOptions([]);
                setModel(undefined);
                setModelLoading(true);
                setPickedHarness(next);
              }}
              disabled={creating}
            />
            <HarnessInstallNote install={install} />
          </div>
          <div className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Model</span>
            <HarnessModelMenu
              harness={harness}
              options={modelOptions}
              value={model}
              onChange={setModel}
              loading={modelLoading}
              disabled={creating}
              variant="field"
            />
          </div>
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Starting prompt</span>
            <Textarea
              value={startingPrompt}
              onChange={(event) => setStartingPrompt(event.target.value)}
              disabled={creating}
              placeholder="Optional"
              rows={3}
            />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Title</span>
            <Input
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              disabled={creating}
              placeholder="Named automatically"
            />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Base ref</span>
            <Input
              value={baseRef}
              onChange={(event) => setBaseRef(event.target.value)}
              disabled={creating}
              placeholder={selectedRepo?.default_base_ref}
            />
          </label>
          <div className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Permission mode</span>
            <Select
              value={postedMode}
              onValueChange={(next) =>
                setPermissionMode(next as PermissionMode)
              }
              disabled={creating || availableModes.length === 0}
            >
              <SelectTrigger aria-label="Permission mode">
                <SelectValue />
              </SelectTrigger>
              <SelectContent scrollButtons={false}>
                {availableModes.map((mode) => (
                  <SelectItem key={mode} value={mode}>
                    {PERMISSION_MODE_LABELS[mode]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {postedMode === "auto" &&
              selectedHarness &&
              autoIsUnsupervised(selectedHarness.caps) && (
                <p className="text-muted-foreground text-xs">
                  {UNSUPERVISED_AUTO_NOTE}
                </p>
              )}
            {postedMode === "allow" && (
              <p className="text-muted-foreground text-xs">{ALLOW_ALL_NOTE}</p>
            )}
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="ghost"
              onClick={() => onOpenChange(false)}
              disabled={creating}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={!canCreate}>
              {creating ? "Creating…" : "Create"}
              {!creating && (
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
        </form>
      </DialogContent>
    </Dialog>
  );
}
