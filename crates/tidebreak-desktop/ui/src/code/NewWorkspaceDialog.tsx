import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import {
  Check,
  ChevronDown,
  Ellipsis,
  FolderGit2,
  GitBranch,
} from "lucide-react";

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
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { shouldSubmitComposerKey } from "../Composer";
import { PermissionModeMenu } from "../PermissionModeMenu";
import { usesCommandModifier } from "@/ShellShortcuts";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore } from "./CodeUiStore";
import { HarnessModelMenu } from "./CodeComposer";
import { HarnessInstallNote } from "./HarnessInstallNote";
import { useWarmHarnessInstall } from "./useHarnessInstall";
import { HARNESS_ICONS } from "./HarnessPicker";
import {
  autoIsUnsupervised,
  createPermissionModes,
  defaultCreatePermissionMode,
  gatewayCodeModels,
  harnessCanStartNow,
  harnessUnusableReason,
  preferredCodeModels,
  requiresHarnessModelIds,
  HARNESS_LABELS,
  ALLOW_ALL_NOTE,
  UNSUPERVISED_AUTO_NOTE,
  type CodeModelOption,
} from "./labels";

/**
 * Create a workspace, its first session, and — when a first message is typed —
 * its first turn, then open it.
 *
 * The dialog is a composer, not a form: the message is the surface, and every
 * setting is a pill that opens on what this reader used last (repo, engine,
 * model, and permission mode stick via `lastCreate`; the catalog covers a
 * fresh window). Enter creates, Shift+Enter breaks a line, and Cmd+Enter
 * creates from anywhere in the dialog, pickers included. Each pill has its own
 * chord — Cmd+N again for the repo, and Alt+E / M / P / B / N for engine,
 * model, permissions, base ref, and name — so a create never needs the mouse.
 *
 * A typed message is posted as the session's first turn once the session
 * exists. If the session or the turn fails, the text is handed to the
 * workspace composer as a draft instead — never dropped. "Create more" keeps
 * the dialog open after a create, clearing only the message and name, for
 * firing off several tasks on the same sticky settings.
 *
 * The title is optional: left blank, the server generates a two-word name and
 * later replaces it with one derived from the first turn, the same way chats
 * are named. Permission mode defaults to the most autonomous posture the
 * engine honors (decision 0039, amended); whichever posture is armed, the
 * note under the message states it. The engine menu lists every doctor entry —
 * ready rows are selectable; unusable ones stay visible, dimmed, with the
 * reason.
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

/**
 * The engine this reader started last, if it can still be started.
 *
 * Falling back to the first selectable row would open the dialog on an engine
 * this machine has never downloaded while an installed one sits below it, so
 * the fallback prefers an engine that can start now.
 */
function recentHarness(
  selectable: readonly HarnessDoctorEntry[],
  sessions: Record<string, CodeSessionSnapshot>,
  remembered: HarnessKind | undefined,
): HarnessKind | undefined {
  const newest = Object.values(sessions).sort((a, b) =>
    b.created_at.localeCompare(a.created_at),
  )[0];
  for (const kind of [newest?.harness_kind, remembered]) {
    if (kind && selectable.some((entry) => entry.kind === kind)) return kind;
  }
  return (
    selectable.find((entry) => harnessCanStartNow(entry))?.kind ??
    selectable[0]?.kind
  );
}

/** Pickers a chord can open; one open at a time, chords toggle. */
type PickerId = "repo" | "engine" | "model" | "mode" | "base" | "name";

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
  const [createMore, setCreateMore] = useState(false);
  const [modelsByHarness, setModelsByHarness] = useState<
    Partial<Record<HarnessKind, string>>
  >({});
  const [modelOptions, setModelOptions] = useState<CodeModelOption[]>([]);
  const [modelLoading, setModelLoading] = useState(false);
  const [openPicker, setOpenPicker] = useState<PickerId | null>(null);
  const openPickerNow = useRef<PickerId | null>(null);
  openPickerNow.current = openPicker;
  const promptInput = useRef<HTMLTextAreaElement>(null);
  const command = useMemo(() => usesCommandModifier(navigator.userAgent), []);

  const allHarnesses = doctor?.harnesses ?? [];
  const selectableHarnesses = allHarnesses.filter(
    (entry) => !harnessUnusableReason(entry),
  );
  // The doctor can land after the dialog opens, so the engine is derived
  // rather than seeded: a pick wins, and until there is one the recent
  // engine follows whatever the report says can be chosen.
  const harness: HarnessKind =
    (pickedHarness && selectableHarnesses.some((e) => e.kind === pickedHarness)
      ? pickedHarness
      : undefined) ??
    recentHarness(selectableHarnesses, sessions, lastCreate?.harness) ??
    "claude_code";
  const model = modelsByHarness[harness];

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
    setCreateMore(false);
    setModelsByHarness({ ...lastCreate?.modelsByHarness });
    setModelOptions([]);
    setModelLoading(false);
    setOpenPicker(null);
    // Reset against the dialog opening, not against catalog refreshes
    // mid-open — a workspace created elsewhere must not move this form.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, defaultRepoId, repos]);

  // A pin this machine has never installed is minutes of npm. Warming it
  // when the dialog opens — and again when the engine changes — moves that
  // off create, where it was a silent stall. The doctor entry is the only
  // trigger: until the report lands, `harness` is the fallback guess rather
  // than anything the reader picked, and downloading on a guess fetches
  // hundreds of megabytes nobody asked for.
  const doctorEntry = allHarnesses.find((item) => item.kind === harness);
  const installed = Boolean(doctorEntry?.found);
  const needsDownload = Boolean(doctorEntry && !doctorEntry.found);
  const install = useWarmHarnessInstall(client, harness, open, needsDownload);

  const selectedRepo = repos.find((repo) => repo.id === repoId);
  const selectedHarness = selectableHarnesses.find(
    (entry) => entry.kind === harness,
  );
  const availableModes = selectedHarness
    ? createPermissionModes(selectedHarness.caps)
    : [];
  const honors = (mode: PermissionMode | null | undefined) =>
    mode && availableModes.includes(mode) ? mode : undefined;
  const postedMode =
    honors(permissionMode) ??
    honors(lastCreate?.permissionMode) ??
    (selectedHarness
      ? defaultCreatePermissionMode(selectedHarness.caps)
      : "plan");
  // An engine still downloading is a legal pick, not a legal start: create
  // would sit on the same npm install with nothing but a spinner. The install
  // note under the pills says what the wait is, and any engine already on
  // disk is one pick away.
  const canCreate =
    Boolean(repoId && selectedRepo && selectedHarness && installed) &&
    !creating;
  const modeNote =
    postedMode === "auto" &&
    selectedHarness &&
    autoIsUnsupervised(selectedHarness.caps)
      ? UNSUPERVISED_AUTO_NOTE
      : postedMode === "allow"
        ? ALLOW_ALL_NOTE
        : null;
  const installNote = install && (!install.done || install.error);

  useEffect(() => {
    if (!open || !harness) return;
    // The reader's last model wins where it is still on offer; otherwise the
    // catalog's default, then the first row. `installed` is a dependency
    // because an engine still downloading has no CLI to list models from:
    // the gateway rows show meanwhile, and the native listing is fetched
    // once the pin lands.
    const apply = (listed: CodeModelOption[]) => {
      setModelOptions(listed);
      setModelsByHarness((current) => {
        const remembered = current[harness];
        const picked =
          remembered && listed.some((option) => option.id === remembered)
            ? remembered
            : (listed.find((option) => option.default)?.id ?? listed[0]?.id);
        if (picked === remembered) return current;
        const next = { ...current };
        if (picked) next[harness] = picked;
        else delete next[harness];
        return next;
      });
    };
    const gateway = gatewayCodeModels(models, harness, defaultModelKey);
    const native = useCodeCatalogStore.getState().modelsByHarness[harness];
    const needsNative =
      requiresHarnessModelIds(harness) || gateway.length === 0;
    if (!needsNative) {
      apply(gateway);
      setModelLoading(false);
      return;
    }
    if (native !== undefined) {
      apply(preferredCodeModels(harness, native, gateway));
      setModelLoading(false);
      return;
    }
    if (!installed) {
      // `GET /code/harnesses/{kind}/models` runs the engine's own CLI, so it
      // answers `harness_not_found` until the pin is on disk.
      apply(gateway);
      setModelLoading(false);
      return;
    }
    setModelOptions([]);
    setModelLoading(true);
    let cancelled = false;
    void ensureHarnessModels(client, harness).then((listed) => {
      if (cancelled) return;
      apply(preferredCodeModels(harness, listed, gateway));
      setModelLoading(false);
    });
    return () => {
      cancelled = true;
    };
    // `lastCreate` seeds `modelsByHarness` when the dialog opens. Subscribing
    // this fetch to later writes would undo a deliberate pick after create.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    client,
    defaultModelKey,
    ensureHarnessModels,
    harness,
    installed,
    models,
    open,
  ]);

  function selectModel(next: string) {
    setModelsByHarness((current) => ({ ...current, [harness]: next }));
  }

  function focusPrompt() {
    window.requestAnimationFrame(() => {
      // Clicking from one pill straight to another closes the first and opens
      // the second in one gesture; the freshly opened menu keeps focus.
      if (openPickerNow.current !== null) return;
      promptInput.current?.focus();
    });
  }

  /** One picker open at a time; closing by chord puts focus back on the message. */
  function pickerProps(id: PickerId) {
    return {
      open: openPicker === id,
      onOpenChange: (next: boolean) => {
        setOpenPicker((current) =>
          next ? id : current === id ? null : current,
        );
        if (!next) focusPrompt();
      },
    };
  }

  function togglePicker(id: PickerId) {
    if (openPicker === id) {
      setOpenPicker(null);
      focusPrompt();
      return;
    }
    setOpenPicker(id);
  }

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
      try {
        const gateway = gatewayCodeModels(models, harness, defaultModelKey);
        const native =
          requiresHarnessModelIds(harness) || gateway.length === 0
            ? await ensureHarnessModels(client, harness)
            : [];
        const listed = preferredCodeModels(harness, native, gateway);
        const posted =
          model ?? listed.find((option) => option.default)?.id ?? listed[0]?.id;
        const session = await client.createCodeSession(workspace.id, {
          harness,
          permission_mode: postedMode,
          model: posted,
        });
        rememberSession(session);
        rememberCreate({
          repoId,
          harness,
          model: posted,
          modelsByHarness,
          permissionMode: postedMode,
        });
        if (prompt) {
          try {
            await client.submitCodeTurn(session.id, prompt);
          } catch (error) {
            // Never drop typed words: the workspace composer holds them.
            useCodeUiStore.getState().offerComposerPrompt(workspace.id, prompt);
            toast.error(
              `Session started, but the first message could not be sent. ${friendlyErrorMessage(error, "Send it from the workspace composer.")}`,
            );
          }
        }
      } catch (error) {
        // No session to send to; the workspace composer holds the text and
        // start-session on the workspace page picks it up.
        if (prompt) {
          useCodeUiStore.getState().offerComposerPrompt(workspace.id, prompt);
        }
        toast.error(
          `Workspace created, but the session could not start. ${friendlyErrorMessage(error, "Try again from the workspace.")}`,
        );
      }
      if (createMore) {
        // Stay here on the same settings; only what named this task clears.
        setStartingPrompt("");
        setTitle("");
        focusPrompt();
        const workspaceId = workspace.id;
        toast.success(`Started ${workspace.title}`, {
          action: {
            label: "Open",
            onClick: () =>
              void navigate({
                to: "/code/w/$workspaceId",
                params: { workspaceId },
              }),
          },
        });
        return;
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

  const HarnessIcon = HARNESS_ICONS[harness];
  const anyUnusable = allHarnesses.some((entry) =>
    harnessUnusableReason(entry),
  );
  // Typed as a plain string: the settings child route is not in the
  // registered route union, the same escape HarnessPicker uses.
  const harnessesPath: string = "/settings/coding-harnesses";
  const shownBaseRef =
    baseRef.trim() || selectedRepo?.default_base_ref || "base";
  const alt = (key: string) => (command ? `⌥${key}` : `Alt+${key}`);

  return (
    <Dialog open={open} onOpenChange={creating ? undefined : onOpenChange}>
      <DialogContent
        className="max-w-3xl gap-0 overflow-hidden p-0 sm:rounded-xl"
        withCloseButton={false}
        aria-busy={creating}
        aria-describedby={undefined}
        onOpenAutoFocus={(event) => {
          // Prompt-centric: the message is where a create starts, so it is
          // where focus lands. Settings are pills a chord away.
          event.preventDefault();
          promptInput.current?.focus();
        }}
        onKeyDownCapture={(event) => {
          // Cmd reads as Ctrl off macOS, and either chord is accepted on
          // both: the dialog is one surface, not worth a per-platform miss.
          const mod = event.metaKey || event.ctrlKey;
          if (event.key === "Enter") {
            // Cmd+Enter creates with what is on screen, whichever element has
            // focus. An open picker portals its list out of this element, but
            // React still routes the key through here, so the chord works
            // with a dropdown up as well.
            if (mod && !event.altKey && !event.shiftKey) {
              event.preventDefault();
              void create();
            }
            return;
          }
          if (creating) return;
          // Every pill has a chord, so a create never needs the mouse. These
          // ride on `event.code`: on macOS an Option chord types an accented
          // character, and `key` would carry that instead of the letter.
          if (
            mod &&
            !event.altKey &&
            !event.shiftKey &&
            event.code === "KeyN"
          ) {
            event.preventDefault();
            togglePicker("repo");
            return;
          }
          if (!event.altKey || mod || event.shiftKey) return;
          const chord: Partial<Record<string, PickerId>> = {
            KeyE: "engine",
            KeyM: "model",
            KeyP: "mode",
            KeyB: "base",
            KeyN: "name",
          };
          const picker = chord[event.code];
          if (!picker) return;
          event.preventDefault();
          togglePicker(picker);
        }}
      >
        <DialogTitle className="sr-only">New workspace</DialogTitle>
        <form className="flex min-w-0 flex-col" onSubmit={submit}>
          <div className="flex min-w-0 items-center gap-1 px-3 pt-3">
            <DropdownMenu {...pickerProps("repo")}>
              <WithTooltip label={`Repo · ${command ? "⌘N" : "Ctrl+N"}`}>
                <DropdownMenuTrigger asChild>
                  <Button
                    type="button"
                    variant="secondary"
                    className="h-8 max-w-64 gap-2 px-2.5"
                    disabled={creating || repos.length === 0}
                    aria-label="Repo"
                  >
                    <FolderGit2 className="size-4 shrink-0 opacity-70" />
                    <span className="truncate">
                      {selectedRepo?.display_name ?? "No repos"}
                    </span>
                    <ChevronDown className="size-4 shrink-0 opacity-50" />
                  </Button>
                </DropdownMenuTrigger>
              </WithTooltip>
              <DropdownMenuContent align="start" className="z-[60] w-64">
                {repos.map((repo) => (
                  <DropdownMenuItem
                    key={repo.id}
                    onSelect={() => {
                      setRepoId(repo.id);
                      setBaseRef(repo.default_base_ref);
                    }}
                    className="flex items-center gap-2"
                  >
                    <FolderGit2 className="text-muted-foreground size-4 shrink-0" />
                    <span className="min-w-0 flex-1 truncate">
                      {repo.display_name}
                    </span>
                    {repo.id === repoId && (
                      <Check className="size-4 shrink-0" />
                    )}
                  </DropdownMenuItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
            <Popover {...pickerProps("name")}>
              <WithTooltip label={`Name · ${alt("N")}`}>
                <PopoverTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    className="text-muted-foreground h-8 max-w-48 gap-1.5 px-2"
                    disabled={creating}
                    aria-label="Workspace name"
                  >
                    <Ellipsis className="size-4 shrink-0" />
                    {title.trim() && (
                      <span className="text-foreground truncate text-xs">
                        {title.trim()}
                      </span>
                    )}
                  </Button>
                </PopoverTrigger>
              </WithTooltip>
              <PopoverContent
                align="start"
                className="z-[60] flex w-72 flex-col gap-1.5 p-3"
              >
                <span className="text-xs font-medium">Name</span>
                <Input
                  value={title}
                  onChange={(event) => setTitle(event.target.value)}
                  placeholder="Named automatically"
                  aria-label="Name"
                  onKeyDown={(event) => {
                    if (event.key !== "Enter" || event.metaKey || event.ctrlKey)
                      return;
                    event.preventDefault();
                    togglePicker("name");
                  }}
                />
                <p className="text-muted-foreground text-xs">
                  Left blank, the first turn names it.
                </p>
              </PopoverContent>
            </Popover>
            <div className="min-w-4 flex-1" />
            <Popover {...pickerProps("base")}>
              <WithTooltip label={`Base ref · ${alt("B")}`}>
                <PopoverTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    className="text-muted-foreground h-8 max-w-56 gap-1.5 px-2"
                    disabled={creating || !selectedRepo}
                    aria-label="Base ref"
                  >
                    <GitBranch className="size-4 shrink-0" />
                    <span className="truncate">From {shownBaseRef}</span>
                    <ChevronDown className="size-4 shrink-0 opacity-50" />
                  </Button>
                </PopoverTrigger>
              </WithTooltip>
              <PopoverContent
                align="end"
                className="z-[60] flex w-72 flex-col gap-1.5 p-3"
              >
                <span className="text-xs font-medium">Base ref</span>
                <Input
                  value={baseRef}
                  onChange={(event) => setBaseRef(event.target.value)}
                  placeholder={selectedRepo?.default_base_ref}
                  aria-label="Base ref"
                  onKeyDown={(event) => {
                    if (event.key !== "Enter" || event.metaKey || event.ctrlKey)
                      return;
                    event.preventDefault();
                    togglePicker("base");
                  }}
                />
                <p className="text-muted-foreground text-xs">
                  The branch, tag, or commit the worktree is cut from.
                </p>
              </PopoverContent>
            </Popover>
          </div>
          <textarea
            ref={promptInput}
            value={startingPrompt}
            onChange={(event) => setStartingPrompt(event.target.value)}
            disabled={creating}
            aria-label="First message"
            placeholder="Describe the first task (optional)"
            className="placeholder:text-muted-foreground max-h-[45vh] min-h-36 w-full resize-none bg-transparent px-4 py-3 text-base outline-none"
            onKeyDown={(event) => {
              if (!shouldSubmitComposerKey(event.nativeEvent)) return;
              event.preventDefault();
              void create();
            }}
          />
          {(installNote || modeNote) && (
            <div className="flex flex-col gap-1 px-4 pb-1.5">
              <HarnessInstallNote install={install} />
              {modeNote && (
                <p className="text-muted-foreground text-xs">{modeNote}</p>
              )}
            </div>
          )}
          <div className="flex min-w-0 items-center gap-1 px-3 pb-3">
            <DropdownMenu {...pickerProps("engine")}>
              <WithTooltip label={`Engine · ${alt("E")}`}>
                <DropdownMenuTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    className="h-8 min-w-0 max-w-48 gap-2 px-2"
                    disabled={creating || allHarnesses.length === 0}
                    aria-label={`Harness: ${HARNESS_LABELS[harness]}`}
                  >
                    <HarnessIcon className="size-4 shrink-0" />
                    <span className="truncate">{HARNESS_LABELS[harness]}</span>
                    <ChevronDown className="size-4 shrink-0 opacity-50" />
                  </Button>
                </DropdownMenuTrigger>
              </WithTooltip>
              <DropdownMenuContent
                align="start"
                side="top"
                className="z-[60] w-64"
              >
                {allHarnesses.map((entry) => {
                  const reason = harnessUnusableReason(entry);
                  const Icon = HARNESS_ICONS[entry.kind];
                  return (
                    <DropdownMenuItem
                      key={entry.kind}
                      disabled={Boolean(reason)}
                      onSelect={() => {
                        setModelOptions([]);
                        setModelLoading(true);
                        setPickedHarness(entry.kind);
                      }}
                      className="flex items-start gap-2.5"
                    >
                      <Icon className="mt-0.5 size-4 shrink-0" />
                      <span className="flex min-w-0 flex-1 flex-col">
                        <span className="truncate font-medium">
                          {HARNESS_LABELS[entry.kind]}
                        </span>
                        {reason && (
                          <span className="text-muted-foreground text-xs">
                            {reason}
                          </span>
                        )}
                      </span>
                      {entry.kind === harness && (
                        <Check className="size-4 shrink-0" />
                      )}
                    </DropdownMenuItem>
                  );
                })}
                {anyUnusable && (
                  <>
                    <DropdownMenuSeparator />
                    <DropdownMenuItem
                      onSelect={() => void navigate({ to: harnessesPath })}
                      className="text-muted-foreground text-sm"
                    >
                      Coding harnesses…
                    </DropdownMenuItem>
                  </>
                )}
              </DropdownMenuContent>
            </DropdownMenu>
            {(modelOptions.length > 0 || modelLoading) && (
              <WithTooltip label={`Model · ${alt("M")}`}>
                <span className="inline-flex min-w-0">
                  <HarnessModelMenu
                    harness={harness}
                    options={modelOptions}
                    value={model}
                    onChange={selectModel}
                    loading={modelLoading}
                    disabled={creating}
                    {...pickerProps("model")}
                  />
                </span>
              </WithTooltip>
            )}
            <WithTooltip label={`Permissions · ${alt("P")}`}>
              <span className="inline-flex min-w-0">
                <PermissionModeMenu
                  scopeKey="code-create"
                  value={postedMode}
                  disabled={creating || availableModes.length === 0}
                  availableModes={availableModes}
                  onChange={(mode) => setPermissionMode(mode)}
                  {...pickerProps("mode")}
                />
              </span>
            </WithTooltip>
            <div className="min-w-4 flex-1" />
            <WithTooltip label="Stay here after create to fire off another">
              <label
                className={cn(
                  "flex h-8 shrink-0 cursor-pointer items-center gap-2 px-2 text-sm",
                  createMore ? "text-foreground" : "text-muted-foreground",
                  creating && "cursor-not-allowed opacity-50",
                )}
              >
                <Switch
                  checked={createMore}
                  onCheckedChange={setCreateMore}
                  disabled={creating}
                  aria-label="Create more"
                  className="h-5 w-9"
                  thumbClassName="size-4 data-[state=checked]:translate-x-4"
                />
                <span className="truncate">Create more</span>
              </label>
            </WithTooltip>
            <Button
              type="submit"
              disabled={!canCreate}
              className="h-8 shrink-0"
            >
              {creating ? "Creating…" : "Create"}
              {!creating && (
                <kbd
                  className="font-sans text-2xs font-medium opacity-60"
                  aria-hidden="true"
                >
                  ↩
                </kbd>
              )}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
