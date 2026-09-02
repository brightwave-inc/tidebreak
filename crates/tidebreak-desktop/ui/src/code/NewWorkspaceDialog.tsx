import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent,
  type FormEvent,
} from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { toast } from "sonner";
import {
  Check,
  ChevronDown,
  Ellipsis,
  FolderGit2,
  GitBranch,
  Plus,
} from "lucide-react";

import type {
  PermissionMode,
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessDoctorEntry,
  HarnessKind,
  ReasoningEffort,
} from "../api/types";
import { HttpError } from "../api/client";
import { useApp } from "@/AppContext";
import { ImageAttachmentList, shouldSubmitComposerKey } from "../Composer";
import {
  imageAttachmentName,
  imageAttachmentRejection,
  imageFilesFrom,
  queuedImageAttachment,
  type ImageAttachment,
} from "../ImageAttachments";
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
  PopoverAnchor,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { clampPermissionMode, PermissionModeMenu } from "../PermissionModeMenu";
import { useManagedPolicy } from "../managedPolicy";
import { usesCommandModifier } from "@/ShellShortcuts";
import {
  OPTIMISTIC_WORKSPACE_ID_PREFIX,
  useCodeCatalogStore,
} from "./CodeCatalogStore";
import { EMPTY_NEW_WORKSPACE_DRAFT, useCodeUiStore } from "./CodeUiStore";
import { AddRepoInline } from "./AddRepoInline";
import { useAddRepoInline } from "./useAddRepoInline";
import {
  FastModeToggle,
  HarnessModelMenu,
  ReasoningEffortMenu,
} from "./CodeComposer";
import { HarnessInstallNote } from "./HarnessInstallNote";
import { useWarmHarnessInstall } from "./useHarnessInstall";
import { startFirstSession } from "./startWorkspaceSession";
import { HARNESS_ICONS } from "./HarnessPicker";
import {
  createPermissionModes,
  defaultCreatePermissionMode,
  effortLadder,
  gatewayCodeModels,
  harnessCanStartNow,
  harnessUnusableReason,
  PERMISSION_MODE_POLICY_BLOCKED,
  preferredCodeModels,
  CREATE_PERMISSION_MODE_FIXED,
  HARNESS_LABELS,
  type CodeModelOption,
} from "./labels";

const NO_ENGINE_EFFORTS: ReasoningEffort[] = [];

/**
 * Create a workspace, its first session, and — when a first message is typed —
 * its first turn, then open it.
 *
 * The dialog is a composer, not a form: the message is the surface, and every
 * setting is a pill that opens on what this reader used last (repo, engine,
 * model, permission mode, reasoning effort, and fast mode stick via
 * `lastCreate`; the catalog covers a
 * fresh window). Enter creates, Shift+Enter breaks a line, and Cmd+Enter
 * creates from anywhere in the dialog, pickers included. Each pill has its own
 * chord — Cmd+N again for the repo, and Alt+E / M / P / B / N for engine,
 * model, permissions, base ref, and name — so a create never needs the mouse.
 *
 * A typed message is posted as the session's first turn once the session
 * exists. If the session or the turn fails, the text is handed to the
 * workspace composer as a draft instead — never dropped. Dismissing the
 * dialog keeps the message and name for the next Cmd+N; a create consumes
 * them. "Create more" keeps the dialog open after a create, clearing only
 * the message and name, for firing off several tasks on the same sticky
 * settings. A create that is not "Create more" opens the workspace as soon
 * as it exists, so the reader is already on it while the first session starts.
 *
 * The title is optional: left blank, the server generates a two-word name and
 * later replaces it with one derived from the first turn, the same way chats
 * are named. Permission mode defaults to the most autonomous posture the
 * engine honors (decision 0039, amended). The engine menu lists every doctor
 * entry — ready rows are selectable; unusable ones stay visible, dimmed, with
 * the reason.
 *
 * The repo pill registers one too. With nothing in the catalog it opens the
 * add-repo field directly, and that field's submit is this dialog's submit:
 * the repo is registered or cloned, and the create chain continues on it. A
 * first run is one message and one submit, not a palette round trip.
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
type PickerId =
  | "repo"
  | "addRepo"
  | "engine"
  | "model"
  | "mode"
  | "base"
  | "name";

type CreateAttempt = {
  repoId: string;
  title: string;
  baseRef: string;
  startingPrompt: string;
  harness: HarnessKind;
  permissionMode: PermissionMode;
  model: string | undefined;
  modelsByHarness: Partial<Record<HarnessKind, string>>;
  reasoningEffort: ReasoningEffort | null;
  reasoningEffortByHarness: Partial<Record<HarnessKind, ReasoningEffort>>;
  fastMode: boolean;
  fastModeByHarness: Partial<Record<HarnessKind, boolean>>;
  createMore: boolean;
  images: readonly File[];
};

type HeldWorkspaceImage = {
  attachment: ImageAttachment;
  file: File;
};

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
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const pathnameRef = useRef(pathname);
  pathnameRef.current = pathname;
  const { client, models, defaultModelKey } = useApp();
  const doctor = useCodeCatalogStore((state) => state.doctor);
  const sessions = useCodeCatalogStore((state) => state.sessionsByWorkspace);
  const upsertWorkspace = useCodeCatalogStore((state) => state.upsertWorkspace);
  const replaceWorkspace = useCodeCatalogStore(
    (state) => state.replaceWorkspace,
  );
  const removeWorkspace = useCodeCatalogStore((state) => state.removeWorkspace);
  const ensureHarnessModels = useCodeCatalogStore(
    (state) => state.ensureHarnessModels,
  );
  const lastCreate = useCodeUiStore((state) => state.lastCreate);
  const rememberCreate = useCodeUiStore((state) => state.rememberCreate);
  const draft = useCodeUiStore((state) => state.newWorkspaceDraft);
  const startingPrompt = draft.startingPrompt;
  const title = draft.title;
  const [repoId, setRepoId] = useState("");
  const [baseRef, setBaseRef] = useState("");
  const [pickedHarness, setPickedHarness] = useState<HarnessKind | null>(null);
  const [permissionMode, setPermissionMode] = useState<PermissionMode | null>(
    null,
  );
  const [createMore, setCreateMore] = useState(false);
  const [heldImages, setHeldImages] = useState<HeldWorkspaceImage[]>([]);
  const [imageError, setImageError] = useState<string | null>(null);
  const heldImagesRef = useRef<HeldWorkspaceImage[]>([]);
  const [modelsByHarness, setModelsByHarness] = useState<
    Partial<Record<HarnessKind, string>>
  >({});
  const [effortByHarness, setEffortByHarness] = useState<
    Partial<Record<HarnessKind, ReasoningEffort>>
  >({});
  const [fastByHarness, setFastByHarness] = useState<
    Partial<Record<HarnessKind, boolean>>
  >({});
  const [modelOptions, setModelOptions] = useState<CodeModelOption[]>([]);
  const [modelLoading, setModelLoading] = useState(false);
  const [openPicker, setOpenPicker] = useState<PickerId | null>(null);
  const openPickerNow = useRef<PickerId | null>(null);
  openPickerNow.current = openPicker;
  const promptInput = useRef<HTMLTextAreaElement>(null);
  const createLocked = useRef(false);
  const retryAttempt = useRef<CreateAttempt | null>(null);
  const command = useMemo(() => usesCommandModifier(navigator.userAgent), []);
  // A repo registered from this dialog reaches the catalog before the prop
  // carrying it comes back around, and the create that follows must not wait
  // a render for it.
  const [addedRepo, setAddedRepo] = useState<CodeRepoSnapshot | null>(null);
  // The repo this reader chose during this opening. A catalog that grows
  // mid-opening — which is exactly what adding a repo does — must not reseed
  // the form they are already filling in.
  const pickedRepo = useRef<string | null>(null);
  const knownRepos = useMemo(
    () =>
      addedRepo && !repos.some((repo) => repo.id === addedRepo.id)
        ? [...repos, addedRepo]
        : repos,
    [addedRepo, repos],
  );

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
  const engineEfforts =
    useCodeCatalogStore((state) => state.effortsByHarness[harness]) ??
    NO_ENGINE_EFFORTS;
  const selectedOption =
    modelOptions.find((option) => option.id === model) ??
    modelOptions.find((option) => option.default) ??
    modelOptions[0];
  const effortLevels = effortLadder(selectedOption, engineEfforts);
  const fastModeAvailable = selectedOption?.fast_mode ?? false;
  const rememberedEffort = effortByHarness[harness];
  const postedEffort =
    rememberedEffort && effortLevels.includes(rememberedEffort)
      ? rememberedEffort
      : null;
  const postedFastMode = fastModeAvailable
    ? Boolean(fastByHarness[harness])
    : false;

  useEffect(() => {
    if (!open) {
      pickedRepo.current = null;
      setAddedRepo(null);
      return;
    }
    // A repo picked or added in this opening is the reader's answer. Catalog
    // churn after that — including the repo they just added — reseeds nothing.
    if (pickedRepo.current) return;
    createLocked.current = false;
    const retry = retryAttempt.current;
    retryAttempt.current = null;
    const { workspaces: known } = useCodeCatalogStore.getState();
    const nextRepo =
      retry?.repoId ??
      defaultRepoId ??
      recentRepoId(knownRepos, known, lastCreate?.repoId);
    setRepoId(nextRepo);
    if (retry) {
      useCodeUiStore.getState().setNewWorkspaceDraft({
        startingPrompt: retry.startingPrompt,
        title: retry.title,
      });
      replaceHeldImages(retry.images);
    }
    setBaseRef(
      retry?.baseRef ??
        knownRepos.find((repo) => repo.id === nextRepo)?.default_base_ref ??
        "",
    );
    setPickedHarness(retry?.harness ?? null);
    setPermissionMode(retry?.permissionMode ?? null);
    setCreateMore(retry?.createMore ?? false);
    setModelsByHarness(
      retry?.modelsByHarness ?? { ...lastCreate?.modelsByHarness },
    );
    setEffortByHarness(
      retry?.reasoningEffortByHarness ?? {
        ...lastCreate?.reasoningEffortByHarness,
      },
    );
    setFastByHarness(
      retry?.fastModeByHarness ?? { ...lastCreate?.fastModeByHarness },
    );
    setModelOptions([]);
    setModelLoading(false);
    setOpenPicker(null);
    // Reset against the dialog opening, not against catalog refreshes
    // mid-open — a workspace created elsewhere must not move this form.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, defaultRepoId, knownRepos]);

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

  const selectedRepo = knownRepos.find((repo) => repo.id === repoId);
  const selectedHarness = selectableHarnesses.find(
    (entry) => entry.kind === harness,
  );
  const availableModes = selectedHarness
    ? createPermissionModes(selectedHarness.caps)
    : [];
  const honors = (mode: PermissionMode | null | undefined) =>
    mode && availableModes.includes(mode) ? mode : undefined;
  const ceiling = useManagedPolicy().permission_mode_ceiling;
  const requestedMode =
    honors(permissionMode) ??
    honors(lastCreate?.permissionMode) ??
    (selectedHarness
      ? defaultCreatePermissionMode(selectedHarness.caps)
      : "plan");
  const permittedMode = clampPermissionMode(
    requestedMode,
    ceiling,
    availableModes,
  );
  const postedMode = permittedMode ?? requestedMode;
  const policyBlocksCreate = Boolean(selectedHarness && permittedMode === null);
  // An engine still downloading is a legal pick, not a legal start: create
  // would sit on the same npm install with nothing but a spinner. The install
  // note under the pills says what the wait is, and any engine already on
  // disk is one pick away.
  //
  // Split from the repo so a repo added inline can create in the same submit,
  // before the catalog prop carrying it has come back around.
  const engineReady = Boolean(
    selectedHarness && installed && !policyBlocksCreate,
  );
  const imageNeedsMessage =
    heldImages.length > 0 && startingPrompt.trim().length === 0;
  const canCreate = Boolean(
    repoId && selectedRepo && engineReady && !imageNeedsMessage,
  );
  const installNote = install && (!install.done || install.error);

  useEffect(
    () => () => {
      for (const image of heldImagesRef.current) {
        if (image.attachment.previewUrl) {
          URL.revokeObjectURL(image.attachment.previewUrl);
        }
      }
    },
    [],
  );

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
    // Fetch the harness listing even when the gateway catalog already has
    // display rows: those rows omit `fast_mode`, and the engine's effort
    // ladder is also stored from this fetch. Join happens in
    // `preferredCodeModels`.
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

  function heldImagesFrom(files: readonly File[]): HeldWorkspaceImage[] {
    const now = new Date();
    return files.map((file) => {
      const previewUrl =
        typeof URL.createObjectURL === "function"
          ? URL.createObjectURL(file)
          : null;
      return {
        file,
        attachment: queuedImageAttachment(crypto.randomUUID(), {
          name: imageAttachmentName(file, now),
          byteLen: file.size,
          previewUrl,
        }),
      };
    });
  }

  function replaceHeldImages(files: readonly File[]) {
    for (const image of heldImagesRef.current) {
      if (image.attachment.previewUrl) {
        URL.revokeObjectURL(image.attachment.previewUrl);
      }
    }
    const next = heldImagesFrom(files);
    heldImagesRef.current = next;
    setHeldImages(next);
    setImageError(null);
  }

  function attachImages(files: readonly File[]) {
    const rejection = imageAttachmentRejection(
      heldImagesRef.current.map((image) => image.attachment),
      files,
    );
    if (rejection) {
      setImageError(rejection);
      return;
    }
    const next = [...heldImagesRef.current, ...heldImagesFrom(files)];
    heldImagesRef.current = next;
    setHeldImages(next);
    setImageError(null);
  }

  function removeImage(id: string) {
    const removed = heldImagesRef.current.find(
      (image) => image.attachment.id === id,
    );
    if (removed?.attachment.previewUrl) {
      URL.revokeObjectURL(removed.attachment.previewUrl);
    }
    const next = heldImagesRef.current.filter(
      (image) => image.attachment.id !== id,
    );
    heldImagesRef.current = next;
    setHeldImages(next);
    setImageError(null);
  }

  function clearImages() {
    for (const image of heldImagesRef.current) {
      if (image.attachment.previewUrl) {
        URL.revokeObjectURL(image.attachment.previewUrl);
      }
    }
    heldImagesRef.current = [];
    setHeldImages([]);
    setImageError(null);
  }

  function onPromptPaste(event: ClipboardEvent<HTMLTextAreaElement>) {
    const files = imageFilesFrom(event.clipboardData);
    if (files.length === 0) return;
    event.preventDefault();
    attachImages(files);
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

  function create(added?: CodeRepoSnapshot) {
    // A repo handed in came from the add-repo field this submit started in,
    // and carries its own base ref: `baseRef` still holds the old repo's.
    const repo = added ?? selectedRepo;
    if (
      !engineReady ||
      !repo ||
      createLocked.current ||
      (heldImagesRef.current.length > 0 && !startingPrompt.trim())
    )
      return;
    createLocked.current = true;
    const attempt: CreateAttempt = {
      repoId: repo.id,
      title,
      baseRef: added ? added.default_base_ref : baseRef,
      startingPrompt,
      harness,
      permissionMode: postedMode,
      model,
      modelsByHarness: { ...modelsByHarness },
      reasoningEffort: postedEffort,
      reasoningEffortByHarness: { ...effortByHarness },
      fastMode: postedFastMode,
      fastModeByHarness: { ...fastByHarness },
      createMore,
      images: heldImagesRef.current.map((image) => image.file),
    };
    useCodeUiStore.getState().setNewWorkspaceDraft(EMPTY_NEW_WORKSPACE_DRAFT);
    clearImages();
    if (createMore) {
      focusPrompt();
      queueMicrotask(() => {
        createLocked.current = false;
      });
    } else {
      onOpenChange(false);
    }
    startCreateAttempt(attempt, repo);
  }

  /**
   * Take the repo the add-repo field just registered, then keep going.
   *
   * The whole point of the field is that adding a repo is not its own errand:
   * the first message is already typed, so this submit continues into the
   * same create chain. An engine that cannot start yet is the one case that
   * stops here, and it leaves the repo picked and the message intact.
   */
  function repoAdded(repo: CodeRepoSnapshot) {
    pickedRepo.current = repo.id;
    setAddedRepo(repo);
    setRepoId(repo.id);
    setBaseRef(repo.default_base_ref);
    setOpenPicker(null);
    if (engineReady) {
      create(repo);
      return;
    }
    focusPrompt();
  }

  function startCreateAttempt(
    attempt: CreateAttempt,
    selectedRepo?: CodeRepoSnapshot,
  ) {
    const repo =
      selectedRepo ??
      knownRepos.find((candidate) => candidate.id === attempt.repoId);
    if (!repo) {
      toast.error("Could not create the workspace because its repo is gone");
      return;
    }
    const pending: CodeWorkspaceSnapshot = {
      id: `${OPTIMISTIC_WORKSPACE_ID_PREFIX}${crypto.randomUUID()}`,
      repo_id: attempt.repoId,
      title: attempt.title.trim() || "New workspace",
      worktree_path: "",
      branch_name: "",
      base_ref: attempt.baseRef.trim() || repo.default_base_ref,
      status: "creating",
      created_at: new Date().toISOString(),
    };
    upsertWorkspace(pending);
    void finishCreate(attempt, pending);
  }

  /**
   * Find the row `POST /code/workspaces` wrote before its setup script failed.
   *
   * The response carried the error, not the workspace, so the id has to come
   * back off the list. Every row already in the catalog is one we know about,
   * so the newest row that is not is the one this attempt created.
   */
  async function findCreatedWorkspace(
    attempt: CreateAttempt,
    pending: CodeWorkspaceSnapshot,
  ): Promise<CodeWorkspaceSnapshot | null> {
    const known = new Set(
      useCodeCatalogStore
        .getState()
        .workspaces.map((workspace) => workspace.id),
    );
    known.delete(pending.id);
    try {
      const listed = await client.listCodeWorkspaces(attempt.repoId);
      const fresh = listed
        .filter((workspace) => !known.has(workspace.id))
        .sort((left, right) => right.created_at.localeCompare(left.created_at));
      return fresh[0] ?? null;
    } catch {
      return null;
    }
  }

  async function finishCreate(
    attempt: CreateAttempt,
    pending: CodeWorkspaceSnapshot,
  ) {
    let workspace: CodeWorkspaceSnapshot;
    try {
      workspace = await client.createCodeWorkspace({
        repo_id: attempt.repoId,
        title: attempt.title.trim() || undefined,
        base_ref: attempt.baseRef.trim() || undefined,
      });
    } catch (error) {
      // A failed setup script still cut the worktree and wrote the workspace
      // row (Decision 0032). Deleting the card and offering "Try again" would
      // create a second worktree for work the user already has, so keep the
      // card, refetch the row the server wrote, and open it.
      if (error instanceof HttpError && error.kind === "setup_failed") {
        const created = await findCreatedWorkspace(attempt, pending);
        toast.error(
          friendlyErrorMessage(error, "Created, but the setup script failed"),
        );
        // No "Try again" either way: the worktree exists, and creating again
        // would cut a second one.
        if (created) {
          replaceWorkspace(pending.id, created);
          await revealCreatedWorkspace(created, attempt);
        } else {
          removeWorkspace(pending.id);
        }
        return;
      }
      removeWorkspace(pending.id);
      retryAttempt.current = attempt;
      if (!attempt.createMore) {
        useCodeUiStore.getState().setNewWorkspaceDraft({
          startingPrompt: attempt.startingPrompt,
          title: attempt.title,
        });
      }
      toast.error(
        friendlyErrorMessage(error, "Could not create the workspace"),
        {
          action: {
            label: "Try again",
            onClick: () => {
              if (retryAttempt.current === attempt) retryAttempt.current = null;
              startCreateAttempt(attempt);
            },
          },
        },
      );
      return;
    }
    replaceWorkspace(pending.id, workspace);
    await startFirstSession({
      client,
      workspace,
      settings: {
        harness: attempt.harness,
        permissionMode: attempt.permissionMode,
        model: attempt.model,
        reasoningEffort: attempt.reasoningEffort,
        fastMode: attempt.fastMode,
      },
      prompt: attempt.startingPrompt,
      images: attempt.images,
      models,
      defaultModelKey,
      reveal: () => revealCreatedWorkspace(workspace, attempt),
      onSessionCreated: (_session, posted) =>
        rememberCreate({
          repoId: attempt.repoId,
          harness: attempt.harness,
          model: posted,
          modelsByHarness: attempt.modelsByHarness,
          permissionMode: attempt.permissionMode,
          reasoningEffort: attempt.reasoningEffort,
          reasoningEffortByHarness: attempt.reasoningEffortByHarness,
          fastMode: attempt.fastMode,
          fastModeByHarness: attempt.fastModeByHarness,
        }),
    });
  }

  async function revealCreatedWorkspace(
    workspace: CodeWorkspaceSnapshot,
    attempt: CreateAttempt,
  ) {
    const workspaceId = workspace.id;
    if (attempt.createMore) {
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
    if (pathnameRef.current === `/code/w/${workspaceId}`) return;
    await navigate({
      to: "/code/w/$workspaceId",
      params: { workspaceId },
    });
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    void create();
  }

  const addRepo = useAddRepoInline({
    open,
    active: openPicker === "addRepo",
    onAdded: repoAdded,
  });
  /** With no repo yet, the pill is the add-repo field rather than a menu. */
  const firstRepo = knownRepos.length === 0;

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
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="max-h-[calc(100dvh-1rem)] max-w-4xl gap-0 overflow-hidden p-0 sm:max-h-[calc(100dvh-3rem)] sm:rounded-xl"
        withCloseButton={false}
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
              // The chord belongs to whatever this submit is finishing. With
              // the add-repo field open, that is registering the repo — which
              // then continues into the create anyway.
              if (openPicker === "addRepo") addRepo.submit();
              else void create();
            }
            return;
          }
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
            togglePicker(firstRepo ? "addRepo" : "repo");
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
        {/* The dialog clips what it cannot fit, and the prompt holds a hard
            floor, so a short window would drop the wrapped footer — the row
            carrying Create — off the bottom. Scrolling the form instead keeps
            every control reachable. */}
        <form
          className="flex min-h-0 min-w-0 flex-col overflow-y-auto"
          onSubmit={submit}
        >
          <div className="flex min-w-0 flex-wrap items-center gap-x-1 gap-y-1 px-3 pt-3">
            <Popover {...pickerProps("addRepo")}>
              {firstRepo ? (
                <WithTooltip
                  label={`Add a repo · ${command ? "⌘N" : "Ctrl+N"}`}
                >
                  <PopoverTrigger asChild>
                    <Button
                      type="button"
                      variant="secondary"
                      className="h-8 max-w-64 gap-2 px-2.5"
                      aria-label="Repo"
                    >
                      <FolderGit2 className="size-4 shrink-0 opacity-70" />
                      <span className="truncate">Add a repo</span>
                      <ChevronDown className="size-4 shrink-0 opacity-50" />
                    </Button>
                  </PopoverTrigger>
                </WithTooltip>
              ) : (
                <PopoverAnchor asChild>
                  <span className="inline-flex min-w-0">
                    <DropdownMenu {...pickerProps("repo")}>
                      <WithTooltip
                        label={`Repo · ${command ? "⌘N" : "Ctrl+N"}`}
                      >
                        <DropdownMenuTrigger asChild>
                          <Button
                            type="button"
                            variant="secondary"
                            className="h-8 max-w-64 gap-2 px-2.5"
                            aria-label="Repo"
                          >
                            <FolderGit2 className="size-4 shrink-0 opacity-70" />
                            <span className="truncate">
                              {selectedRepo?.display_name ?? "No repo"}
                            </span>
                            <ChevronDown className="size-4 shrink-0 opacity-50" />
                          </Button>
                        </DropdownMenuTrigger>
                      </WithTooltip>
                      <DropdownMenuContent
                        align="start"
                        className="z-[60] w-64"
                      >
                        {knownRepos.map((repo) => (
                          <DropdownMenuItem
                            key={repo.id}
                            onSelect={() => {
                              pickedRepo.current = repo.id;
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
                        <DropdownMenuSeparator />
                        <DropdownMenuItem
                          onSelect={() => setOpenPicker("addRepo")}
                          className="flex items-center gap-2"
                        >
                          <Plus className="text-muted-foreground size-4 shrink-0" />
                          <span className="min-w-0 flex-1 truncate">
                            Add a repo…
                          </span>
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </span>
                </PopoverAnchor>
              )}
              <PopoverContent align="start" className="z-[60] w-80 p-3">
                <AddRepoInline
                  state={addRepo}
                  submitLabel={engineReady ? "Add and create" : "Add repo"}
                />
              </PopoverContent>
            </Popover>
            <Popover {...pickerProps("name")}>
              <WithTooltip label={`Name · ${alt("N")}`}>
                <PopoverTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    className="text-muted-foreground h-8 max-w-48 gap-1.5 px-2"
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
                  onChange={(event) => {
                    const current = useCodeUiStore.getState().newWorkspaceDraft;
                    useCodeUiStore.getState().setNewWorkspaceDraft({
                      ...current,
                      title: event.target.value,
                    });
                  }}
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
            <Popover {...pickerProps("base")}>
              <WithTooltip label={`Base ref · ${alt("B")}`}>
                <PopoverTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    className="text-muted-foreground ml-auto h-8 max-w-56 gap-1.5 px-2"
                    disabled={!selectedRepo}
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
            onChange={(event) => {
              const current = useCodeUiStore.getState().newWorkspaceDraft;
              useCodeUiStore.getState().setNewWorkspaceDraft({
                ...current,
                startingPrompt: event.target.value,
              });
            }}
            aria-label="First message"
            placeholder="Describe the first task (optional)"
            className="placeholder:text-muted-foreground max-h-[50vh] min-h-48 w-full resize-none bg-transparent px-4 py-3 text-base outline-none sm:min-h-52"
            onPaste={onPromptPaste}
            onKeyDown={(event) => {
              if (!shouldSubmitComposerKey(event.nativeEvent)) return;
              event.preventDefault();
              void create();
            }}
          />
          {(heldImages.length > 0 || imageError) && (
            <div className="grid gap-1.5 px-4 pb-2">
              <ImageAttachmentList
                items={heldImages.map((image) => image.attachment)}
                onRemove={removeImage}
              />
              {imageError && (
                <p className="text-destructive text-xs" role="alert">
                  {imageError}
                </p>
              )}
              {imageNeedsMessage && (
                <p className="text-muted-foreground text-xs">
                  Add a message to send the image with the first turn.
                </p>
              )}
            </div>
          )}
          {installNote && (
            <div className="flex flex-col gap-1 px-4 pb-1.5">
              <HarnessInstallNote install={install} />
            </div>
          )}
          <div
            className="flex min-w-0 flex-wrap items-end gap-x-2 gap-y-2 px-3 pb-3"
            data-testid="new-workspace-controls"
          >
            <div className="flex min-w-0 flex-1 basis-80 flex-wrap items-center gap-1">
              <DropdownMenu {...pickerProps("engine")}>
                <WithTooltip label={`Engine · ${alt("E")}`}>
                  <DropdownMenuTrigger asChild>
                    <Button
                      type="button"
                      variant="ghost"
                      className="h-8 min-w-0 max-w-48 gap-2 px-2"
                      disabled={allHarnesses.length === 0}
                      aria-label={`Harness: ${HARNESS_LABELS[harness]}`}
                    >
                      <HarnessIcon className="size-4 shrink-0" />
                      <span className="truncate">
                        {HARNESS_LABELS[harness]}
                      </span>
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
                      {...pickerProps("model")}
                    />
                  </span>
                </WithTooltip>
              )}
              {effortLevels.length > 0 && (
                <ReasoningEffortMenu
                  levels={effortLevels}
                  value={postedEffort}
                  onChange={(next) => {
                    setEffortByHarness((current) => {
                      const nextMap = { ...current };
                      if (next) nextMap[harness] = next;
                      else delete nextMap[harness];
                      return nextMap;
                    });
                  }}
                />
              )}
              <FastModeToggle
                available={fastModeAvailable}
                value={postedFastMode}
                onChange={(next) => {
                  setFastByHarness((current) => ({
                    ...current,
                    [harness]: next,
                  }));
                }}
              />
              <div className="flex min-w-0 flex-col">
                <WithTooltip label={`Permissions · ${alt("P")}`}>
                  <span className="inline-flex min-w-0">
                    <PermissionModeMenu
                      scopeKey="code-create"
                      value={postedMode}
                      clampDisplay={false}
                      disabled={
                        availableModes.length === 0 || policyBlocksCreate
                      }
                      availableModes={availableModes}
                      onChange={(mode) => setPermissionMode(mode)}
                      {...pickerProps("mode")}
                    />
                  </span>
                </WithTooltip>
                {selectedHarness?.relaunch_composes_permission_mode ===
                  false && (
                  <p className="text-muted-foreground px-2 text-xs">
                    {CREATE_PERMISSION_MODE_FIXED}
                  </p>
                )}
                {policyBlocksCreate && (
                  <p className="text-muted-foreground px-2 text-xs">
                    {PERMISSION_MODE_POLICY_BLOCKED}
                  </p>
                )}
              </div>
            </div>

            <div className="ml-auto flex shrink-0 items-center gap-1">
              <WithTooltip label="Stay here after create to fire off another">
                <label
                  className={cn(
                    "flex h-8 shrink-0 cursor-pointer items-center gap-2 px-2 text-sm",
                    createMore ? "text-foreground" : "text-muted-foreground",
                  )}
                >
                  <Switch
                    checked={createMore}
                    onCheckedChange={setCreateMore}
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
                Create
                <span
                  className="ml-1 inline-flex items-center gap-0.5 text-2xs font-medium opacity-60"
                  aria-hidden="true"
                >
                  <kbd className="font-sans">{command ? "⌘" : "Ctrl"}</kbd>
                  <kbd className="font-sans">↩</kbd>
                </span>
              </Button>
            </div>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
