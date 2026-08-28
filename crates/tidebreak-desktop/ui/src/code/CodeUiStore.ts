import { create } from "zustand";

import type {
  HarnessKind,
  PermissionMode,
  ReasoningEffort,
} from "../api/types";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import type { StatusTone } from "./statusTone";
import type { WorkflowShortcut } from "./workspaceWorkflow";
import {
  isCardDensity,
  isWorkspaceSortMode,
  type CardDensity,
  type WorkspaceSortMode,
} from "./workspaceCards";

const REVIEW_SIDEBAR_OPEN_KEY = "tidebreak.code-review-sidebar-open";
const LAST_CREATE_KEY = "tidebreak.code-last-create";
const WORKSPACE_SORT_KEY = "tidebreak.code-workspace-sort";
const RAIL_PREFS_KEY = "tidebreak.code-rail-prefs";
const HARNESS_KINDS: readonly HarnessKind[] = [
  "claude_code",
  "codex",
  "opencode",
  "grok",
];
const REASONING_EFFORTS: readonly ReasoningEffort[] = [
  "none",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
];

/**
 * How the reader shaped the workspace rail: order, card density, which meta
 * the cards draw. One blob, one writer —
 * per-key storage is how preferences drift into half-migrated states.
 */
export type CodeRailPrefs = {
  sortMode: WorkspaceSortMode;
  density: CardDensity;
  /** Suppressed per-card in by-repo order regardless; this is the reader's say. */
  showRepoChip: boolean;
  showBranch: boolean;
};

export const DEFAULT_RAIL_PREFS: CodeRailPrefs = {
  sortMode: "by-repo",
  density: "detailed",
  showRepoChip: true,
  showBranch: false,
};

/** What the reader picked the last time they created a workspace. */
export type CodeCreateDefaults = {
  repoId?: string;
  harness?: HarnessKind;
  modelsByHarness: Partial<Record<HarnessKind, string>>;
  permissionMode?: PermissionMode;
  /** Last honored reasoning effort per engine; omitted when the engine has none. */
  reasoningEffortByHarness?: Partial<Record<HarnessKind, ReasoningEffort>>;
  /** Last fast-mode pick per engine; only restored when the model still serves it. */
  fastModeByHarness?: Partial<Record<HarnessKind, boolean>>;
};

/** What one successful create adds to the remembered defaults. */
export type CodeCreateSelection = {
  repoId?: string;
  harness: HarnessKind;
  model?: string;
  /** Picks made before the final harness, kept for the next switch back. */
  modelsByHarness?: Partial<Record<HarnessKind, string>>;
  permissionMode?: PermissionMode;
  reasoningEffort?: ReasoningEffort | null;
  reasoningEffortByHarness?: Partial<Record<HarnessKind, ReasoningEffort>>;
  fastMode?: boolean;
  fastModeByHarness?: Partial<Record<HarnessKind, boolean>>;
};

/**
 * Unsent first message and name in the new-workspace composer. Closing the
 * dialog keeps them; the next Cmd+N restores them. A create consumes them.
 */
export type NewWorkspaceDraft = {
  startingPrompt: string;
  title: string;
};

export const EMPTY_NEW_WORKSPACE_DRAFT: NewWorkspaceDraft = {
  startingPrompt: "",
  title: "",
};

/** Inspector filter for one turn's files and diff. `label` is the ordinal, never the id. */
export type InspectorScope = {
  turnId: string;
  label: string;
};

export type PendingComposerPrompt = {
  scope: string;
  text: string;
  submit: boolean;
  /** Files to attach once a session exists to publish them. */
  images?: readonly File[];
};

export type PendingComposerImages = {
  scope: string;
  files: readonly File[];
};

function readStoredCreateDefaults(): CodeCreateDefaults | null {
  try {
    const raw = window.localStorage.getItem(LAST_CREATE_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return null;
    const record = parsed as Record<string, unknown>;
    const text = (value: unknown) =>
      typeof value === "string" && value.length > 0 ? value : undefined;
    const mode = (value: unknown): PermissionMode | undefined =>
      value === "plan" ||
      value === "ask" ||
      value === "auto" ||
      value === "allow"
        ? value
        : undefined;
    const harness = text(record.harness);
    const rememberedHarness = HARNESS_KINDS.includes(harness as HarnessKind)
      ? (harness as HarnessKind)
      : undefined;
    const modelsByHarness: Partial<Record<HarnessKind, string>> = {};
    const storedModels = record.modelsByHarness;
    if (storedModels && typeof storedModels === "object") {
      const modelRecord = storedModels as Record<string, unknown>;
      for (const kind of HARNESS_KINDS) {
        const model = text(modelRecord[kind]);
        if (model) modelsByHarness[kind] = model;
      }
    }
    // Migrate the earlier one-model shape into the harness it belonged to.
    const legacyModel = text(record.model);
    if (
      rememberedHarness &&
      legacyModel &&
      !modelsByHarness[rememberedHarness]
    ) {
      modelsByHarness[rememberedHarness] = legacyModel;
    }
    const reasoningEffortByHarness: Partial<
      Record<HarnessKind, ReasoningEffort>
    > = {};
    const storedEfforts = record.reasoningEffortByHarness;
    if (storedEfforts && typeof storedEfforts === "object") {
      const effortRecord = storedEfforts as Record<string, unknown>;
      for (const kind of HARNESS_KINDS) {
        const effort = effortRecord[kind];
        if (
          typeof effort === "string" &&
          REASONING_EFFORTS.includes(effort as ReasoningEffort)
        ) {
          reasoningEffortByHarness[kind] = effort as ReasoningEffort;
        }
      }
    }
    const fastModeByHarness: Partial<Record<HarnessKind, boolean>> = {};
    const storedFast = record.fastModeByHarness;
    if (storedFast && typeof storedFast === "object") {
      const fastRecord = storedFast as Record<string, unknown>;
      for (const kind of HARNESS_KINDS) {
        if (typeof fastRecord[kind] === "boolean") {
          fastModeByHarness[kind] = fastRecord[kind];
        }
      }
    }
    return {
      repoId: text(record.repoId),
      harness: rememberedHarness,
      modelsByHarness,
      permissionMode: mode(record.permissionMode),
      reasoningEffortByHarness,
      fastModeByHarness,
    };
  } catch {
    return null;
  }
}

function storeCreateDefaults(defaults: CodeCreateDefaults): void {
  try {
    window.localStorage.setItem(LAST_CREATE_KEY, JSON.stringify(defaults));
  } catch {
    // Preference persistence is best-effort.
  }
}

function readStoredReviewSidebarOpen(): boolean {
  try {
    const raw = window.localStorage.getItem(REVIEW_SIDEBAR_OPEN_KEY);
    // Git, the pull request, and comments live here now — open until the
    // reader hides the rail, then remember that.
    if (raw == null) return true;
    return raw === "true";
  } catch {
    return true;
  }
}

function storeReviewSidebarOpen(open: boolean): void {
  try {
    window.localStorage.setItem(REVIEW_SIDEBAR_OPEN_KEY, String(open));
  } catch {
    // Preference persistence is best-effort.
  }
}

function readStoredWorkspaceSort(): WorkspaceSortMode {
  try {
    const raw = window.localStorage.getItem(WORKSPACE_SORT_KEY);
    if (raw && isWorkspaceSortMode(raw)) return raw;
  } catch {
    // Preference persistence is best-effort.
  }
  return DEFAULT_RAIL_PREFS.sortMode;
}

/**
 * Field-wise: a blob written by a newer build with one more key must not
 * knock the known fields back to defaults. When no blob exists yet, the
 * legacy sort key (the only rail pref that predates the blob) seeds it.
 */
function readStoredRailPrefs(): CodeRailPrefs {
  try {
    const raw = window.localStorage.getItem(RAIL_PREFS_KEY);
    if (!raw) {
      return { ...DEFAULT_RAIL_PREFS, sortMode: readStoredWorkspaceSort() };
    }
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return DEFAULT_RAIL_PREFS;
    const record = parsed as Record<string, unknown>;
    const flag = (value: unknown, fallback: boolean) =>
      typeof value === "boolean" ? value : fallback;
    return {
      sortMode:
        typeof record.sortMode === "string" &&
        isWorkspaceSortMode(record.sortMode)
          ? record.sortMode
          : DEFAULT_RAIL_PREFS.sortMode,
      density:
        typeof record.density === "string" && isCardDensity(record.density)
          ? record.density
          : DEFAULT_RAIL_PREFS.density,
      showRepoChip: flag(record.showRepoChip, DEFAULT_RAIL_PREFS.showRepoChip),
      showBranch: flag(record.showBranch, DEFAULT_RAIL_PREFS.showBranch),
    };
  } catch {
    return DEFAULT_RAIL_PREFS;
  }
}

function storeRailPrefs(prefs: CodeRailPrefs): void {
  try {
    window.localStorage.setItem(RAIL_PREFS_KEY, JSON.stringify(prefs));
  } catch {
    // Preference persistence is best-effort.
  }
}

/**
 * Code mode's dialog and chrome state, held outside the components that draw it.
 *
 * The new-workspace flow is reachable from three places — the rail, a repo
 * page, and Cmd+N — and only one of them is a component the shell can see. A
 * store lets the shortcut open the dialog without the shell importing code
 * mode's surfaces, and keeps a single dialog instance rather than one per
 * button that opens it.
 *
 * The review rail is the same kind of chrome as the left sidebar: it is not
 * a URL, and a reload should not forget whether it was showing. Terminal
 * drawer height is the same kind of chrome, remembered per workspace.
 *
 * `lastCreate` is the tie-breaker for the new-workspace dialog's defaults.
 * The catalog answers "what did you work on last" for anyone with a
 * workspace already; this covers the first run of a window and the reader
 * whose newest workspace is not the one they want to repeat.
 */
export type CodeUiStore = {
  newWorkspaceOpen: boolean;
  /** The repo the dialog opens on, when it was opened from one. */
  newWorkspaceRepoId: string | undefined;
  /**
   * Typed words still sitting in the new-workspace composer after a dismiss.
   * Settings stick via `lastCreate`; this is only the message and name.
   */
  newWorkspaceDraft: NewWorkspaceDraft;
  setNewWorkspaceDraft: (draft: NewWorkspaceDraft) => void;
  addRepoOpen: boolean;
  reviewSidebarOpen: boolean;
  /** Files and diff scoped to one turn, or the whole worktree when null. */
  inspectorScope: InspectorScope | null;
  railPrefs: CodeRailPrefs;
  lastCreate: CodeCreateDefaults | null;
  /**
   * A terminal has been asked for from outside the workspace page — the
   * chord, or a rail command on a workspace that is not on screen yet. The
   * page takes it, because opening a shell is a server call the chord cannot
   * make on its own.
   */
  terminalPending: boolean;
  /**
   * Ask for a workspace, from a repo row or from anywhere in code mode.
   *
   * With nothing registered the new-workspace dialog is a form the reader
   * cannot submit, so the request lands on repo registration instead — that is
   * the step actually in their way.
   */
  startNewWorkspace: (repoId?: string) => void;
  setNewWorkspaceOpen: (open: boolean) => void;
  setAddRepoOpen: (open: boolean) => void;
  toggleReviewSidebar: () => void;
  setReviewSidebarOpen: (open: boolean) => void;
  setInspectorScope: (scope: InspectorScope | null) => void;
  setRailPrefs: (patch: Partial<CodeRailPrefs>) => void;
  /** Record a successful create so the next dialog opens on the same choices. */
  rememberCreate: (selection: CodeCreateSelection) => void;
  requestTerminal: () => void;
  takeTerminal: () => boolean;
  /**
   * Prompt waiting for the code composer. It can either fill the draft or run
   * immediately; the composer takes and clears it so a remount cannot repeat
   * the action.
   */
  pendingComposerPrompt: PendingComposerPrompt | null;
  pendingComposerImages: PendingComposerImages | null;
  composerActionScope: string | null;
  offerComposerPrompt: (
    scope: string,
    prompt: string,
    images?: readonly File[],
  ) => void;
  runComposerPrompt: (scope: string, prompt: string) => boolean;
  takeComposerPrompt: (scope: string) => PendingComposerPrompt | null;
  takeComposerImages: (scope: string) => readonly File[] | null;
  finishComposerAction: (scope: string) => void;
  /**
   * Asks raised by the shell keymap that only a workspace surface can carry
   * out.
   *
   * The shortcut listener lives above the route, so it cannot reach into the
   * workspace's own state; it raises a flag here and the surface that owns the
   * affordance takes it, the same way a queued composer prompt is taken. A
   * flag rather than a counter because the ask does not queue — pressing the
   * chord twice before the panel mounts is still one ask — and taking it is
   * what keeps a later remount from repeating it.
   */
  quickOpenPending: boolean;
  newTabMenuPending: boolean;
  filesSearchPending: boolean;
  requestQuickOpen: () => void;
  takeQuickOpen: () => boolean;
  requestNewTabMenu: () => void;
  takeNewTabMenu: () => boolean;
  /** Opens the review sidebar too: search is dead while that rail is hidden. */
  requestFilesSearch: () => void;
  takeFilesSearch: () => boolean;
  /**
   * The Ship chord waiting for the workspace header, which is the one surface
   * that knows the branch and pull-request state a chord resolves against.
   *
   * Held rather than run here because the shell has none of that state, and
   * because the same chord means different actions at different stages —
   * deciding which in the store would put the decision two layers from the
   * data it depends on.
   *
   * Scoped to the workspace it was pressed on, the way a queued composer
   * prompt is. An archived workspace draws no header control, so a chord
   * pressed there is never taken; without the scope it would sit in the store
   * and fire on the next workspace the reader opened.
   */
  workflowShortcutPending: {
    workspaceId: string;
    shortcut: WorkflowShortcut;
  } | null;
  requestWorkflowShortcut: (
    workspaceId: string,
    shortcut: WorkflowShortcut,
  ) => void;
  takeWorkflowShortcut: (workspaceId: string) => WorkflowShortcut | null;
  /** The archive chord, taken by the workspace page that owns the command. */
  archivePending: boolean;
  requestArchiveWorkspace: () => void;
  takeArchiveWorkspace: () => boolean;
  /**
   * What the workspace header says the next step is, republished for the
   * command palette.
   *
   * The workflow control already resolves this from the branch and pull-request
   * state to label its own primary button. The palette leads with the same
   * answer rather than fetching the snapshot a second time and risking a
   * different one — a palette that offered "Push" while the header said
   * "Merge" would be worse than offering nothing.
   */
  workflowSuggestion: WorkflowSuggestion | null;
  publishWorkflowSuggestion: (suggestion: WorkflowSuggestion | null) => void;
  /**
   * A path the palette picked, taken by the workspace page that owns the tabs.
   *
   * The palette can rank a worktree's files but has nowhere to put one: which
   * editor group a file opens into is the page's business. So it names the
   * path and the page opens it, the same way every other cross-surface ask
   * here works.
   */
  openFilePending: string | null;
  requestOpenFilePath: (path: string) => void;
  takeOpenFilePath: () => string | null;
  /**
   * Ephemeral rail selection. Cmd/Ctrl-click and shift-click write it;
   * unmodified click, Escape, and a bulk archive clear it. Not persisted.
   */
  selectedWorkspaceIds: string[];
  selectionAnchorId: string | null;
  replaceWorkspaceSelection: (
    ids: readonly string[],
    anchorId: string | null,
  ) => void;
  clearWorkspaceSelection: () => void;
};

/** The header's primary action, as the palette's leading row. */
export type WorkflowSuggestion = {
  workspaceId: string;
  label: string;
  summary: string;
  tone: StatusTone;
};

export const useCodeUiStore = create<CodeUiStore>()((set, get) => ({
  newWorkspaceOpen: false,
  newWorkspaceRepoId: undefined,
  newWorkspaceDraft: EMPTY_NEW_WORKSPACE_DRAFT,
  addRepoOpen: false,
  reviewSidebarOpen: readStoredReviewSidebarOpen(),
  inspectorScope: null,
  railPrefs: readStoredRailPrefs(),
  lastCreate: readStoredCreateDefaults(),
  terminalPending: false,
  pendingComposerPrompt: null,
  pendingComposerImages: null,
  composerActionScope: null,
  quickOpenPending: false,
  newTabMenuPending: false,
  filesSearchPending: false,
  requestQuickOpen: () => set({ quickOpenPending: true }),
  takeQuickOpen: () => {
    if (!get().quickOpenPending) return false;
    set({ quickOpenPending: false });
    return true;
  },
  requestNewTabMenu: () => set({ newTabMenuPending: true }),
  takeNewTabMenu: () => {
    if (!get().newTabMenuPending) return false;
    set({ newTabMenuPending: false });
    return true;
  },
  requestFilesSearch: () => {
    storeReviewSidebarOpen(true);
    set({ reviewSidebarOpen: true, filesSearchPending: true });
  },
  takeFilesSearch: () => {
    if (!get().filesSearchPending) return false;
    set({ filesSearchPending: false });
    return true;
  },
  workflowShortcutPending: null,
  // Last chord wins rather than queueing. Two Ship chords in a row is a reader
  // correcting themselves, not asking for both.
  requestWorkflowShortcut: (workspaceId, shortcut) =>
    set({ workflowShortcutPending: { workspaceId, shortcut } }),
  takeWorkflowShortcut: (workspaceId) => {
    const pending = get().workflowShortcutPending;
    if (pending === null || pending.workspaceId !== workspaceId) return null;
    set({ workflowShortcutPending: null });
    return pending.shortcut;
  },
  archivePending: false,
  requestArchiveWorkspace: () => set({ archivePending: true }),
  takeArchiveWorkspace: () => {
    if (!get().archivePending) return false;
    set({ archivePending: false });
    return true;
  },
  workflowSuggestion: null,
  publishWorkflowSuggestion: (workflowSuggestion) =>
    set({ workflowSuggestion }),
  openFilePending: null,
  selectedWorkspaceIds: [],
  selectionAnchorId: null,
  replaceWorkspaceSelection: (ids, anchorId) =>
    set({
      selectedWorkspaceIds: [...ids],
      selectionAnchorId: anchorId,
    }),
  clearWorkspaceSelection: () =>
    set({ selectedWorkspaceIds: [], selectionAnchorId: null }),
  requestOpenFilePath: (path) => set({ openFilePending: path }),
  takeOpenFilePath: () => {
    const pending = get().openFilePending;
    if (pending === null) return null;
    set({ openFilePending: null });
    return pending;
  },
  offerComposerPrompt: (scope, prompt, images) =>
    set({
      pendingComposerPrompt: {
        scope,
        text: prompt,
        submit: false,
        ...(images && images.length > 0 ? { images } : {}),
      },
      pendingComposerImages:
        images && images.length > 0
          ? { scope, files: images }
          : get().pendingComposerImages?.scope === scope
            ? null
            : get().pendingComposerImages,
    }),
  runComposerPrompt: (scope, prompt) => {
    if (get().composerActionScope !== null) return false;
    set({
      pendingComposerPrompt: { scope, text: prompt, submit: true },
      composerActionScope: scope,
    });
    return true;
  },
  takeComposerPrompt: (scope): PendingComposerPrompt | null => {
    const prompt = get().pendingComposerPrompt;
    if (!prompt || prompt.scope !== scope) return null;
    set({ pendingComposerPrompt: null });
    return prompt;
  },
  takeComposerImages: (scope): readonly File[] | null => {
    const held = get().pendingComposerImages;
    if (!held || held.scope !== scope) return null;
    set({ pendingComposerImages: null });
    return held.files;
  },
  finishComposerAction: (scope) =>
    set((state) => {
      if (state.composerActionScope !== scope) return state;
      return {
        composerActionScope: null,
        pendingComposerPrompt:
          state.pendingComposerPrompt?.scope === scope
            ? null
            : state.pendingComposerPrompt,
      };
    }),
  startNewWorkspace: (repoId) => {
    const { repos } = useCodeCatalogStore.getState();
    if (repos.length === 0) {
      set({ addRepoOpen: true });
      return;
    }
    // A repo the catalog does not know is not a useful default; free
    // choice is the safe fallback.
    const known = repos.some((repo) => repo.id === repoId) ? repoId : undefined;
    set({ newWorkspaceOpen: true, newWorkspaceRepoId: known });
  },
  setNewWorkspaceOpen: (open) => set({ newWorkspaceOpen: open }),
  setNewWorkspaceDraft: (newWorkspaceDraft) => set({ newWorkspaceDraft }),
  setAddRepoOpen: (open) => set({ addRepoOpen: open }),
  toggleReviewSidebar: () =>
    set((state) => {
      const reviewSidebarOpen = !state.reviewSidebarOpen;
      storeReviewSidebarOpen(reviewSidebarOpen);
      return { reviewSidebarOpen };
    }),
  setReviewSidebarOpen: (open) => {
    storeReviewSidebarOpen(open);
    set({ reviewSidebarOpen: open });
  },
  setInspectorScope: (inspectorScope) => set({ inspectorScope }),
  setRailPrefs: (patch) =>
    set((state) => {
      const railPrefs = { ...state.railPrefs, ...patch };
      storeRailPrefs(railPrefs);
      return { railPrefs };
    }),
  rememberCreate: (selection) => {
    const previous = get().lastCreate;
    const modelsByHarness = {
      ...previous?.modelsByHarness,
      ...selection.modelsByHarness,
    };
    if (selection.model) {
      modelsByHarness[selection.harness] = selection.model;
    }
    const reasoningEffortByHarness = {
      ...previous?.reasoningEffortByHarness,
      ...selection.reasoningEffortByHarness,
    };
    if (selection.reasoningEffort) {
      reasoningEffortByHarness[selection.harness] = selection.reasoningEffort;
    } else if (selection.reasoningEffort === null) {
      delete reasoningEffortByHarness[selection.harness];
    }
    const fastModeByHarness = {
      ...previous?.fastModeByHarness,
      ...selection.fastModeByHarness,
    };
    if (selection.fastMode !== undefined) {
      fastModeByHarness[selection.harness] = selection.fastMode;
    }
    const defaults: CodeCreateDefaults = {
      repoId: selection.repoId,
      harness: selection.harness,
      modelsByHarness,
      permissionMode: selection.permissionMode,
      reasoningEffortByHarness,
      fastModeByHarness,
    };
    storeCreateDefaults(defaults);
    set({ lastCreate: defaults });
  },
  requestTerminal: () => set({ terminalPending: true }),
  takeTerminal: () => {
    if (!get().terminalPending) return false;
    set({ terminalPending: false });
    return true;
  },
}));

/** Clear Code actions that refer to the authority AppShell just replaced. */
export function resetCodeUiHostState(): void {
  useCodeUiStore.setState({
    newWorkspaceOpen: false,
    newWorkspaceRepoId: undefined,
    newWorkspaceDraft: EMPTY_NEW_WORKSPACE_DRAFT,
    addRepoOpen: false,
    inspectorScope: null,
    terminalPending: false,
    pendingComposerPrompt: null,
    pendingComposerImages: null,
    composerActionScope: null,
    quickOpenPending: false,
    newTabMenuPending: false,
    filesSearchPending: false,
    workflowShortcutPending: null,
    archivePending: false,
    workflowSuggestion: null,
    openFilePending: null,
    selectedWorkspaceIds: [],
    selectionAnchorId: null,
  });
}
