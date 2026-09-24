import {
  useCallback,
  useMemo,
  useRef,
  useState,
  useEffect,
  type ReactNode,
} from "react";
import {
  Check,
  ChevronDown,
  Gauge,
  List,
  Search,
  Sparkles,
  Zap,
} from "lucide-react";

import type {
  PermissionMode,
  HarnessKind,
  ReasoningEffort,
} from "../api/types";
import { useApp } from "@/AppContext";
import { useConfirm } from "@/components/ConfirmDialog";
import {
  Composer,
  type ComposerReviewComments,
  type ComposerWorkspaceFiles,
} from "../Composer";
import {
  useComposerAttachments,
  useComposerDraft,
  useComposerDrafts,
} from "../ComposerDrafts";
import { IMAGE_MEDIA_TYPES } from "../ImageAttachments";
import { moveComposerDraft, useImageAttachments } from "../useImageAttachments";
import {
  messageWithPastedText,
  type PastedTextAttachment,
} from "../PastedText";
import { reasoningEffortOptions } from "../ModelMenu";
import { familyForModelId } from "../modelFamilies";
import { PermissionModeMenu } from "../PermissionModeMenu";
import { ProviderIcon } from "../ProviderIcons";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type { ContextUsageReading } from "../ContextUsageIndicator";
import type { CodeTurnSubmission } from "./parsers";
import {
  codeSendFailure,
  seedCodeComposer,
  sendCodeComposer,
  sendCodeTurn,
  turnNamer,
  useCodeComposerStatus,
  setCodeComposerNotice,
  useCodeComposerSendStatus,
  type CodeTurnImage,
} from "./CodeSessionSend";
import { useCodeUiStore } from "./CodeUiStore";
import {
  commentsReadyToSend,
  reviewSummary,
  usePendingReview,
  usePendingReviewStore,
} from "./diff/pendingReview";
import { messageWithReviewComments } from "./diff/reviewComments";
import {
  codeModelVendor,
  effortLadder,
  groupCodeModelOptions,
  matchingCodeModels,
  type CodeModelOption,
  PERMISSION_MODE_UNAVAILABLE_REASON,
  SESSION_PERMISSION_MODE_LOCKED,
} from "./labels";
import { HARNESS_ICONS } from "./HarnessPicker";
import { STATUS_TEXT } from "./statusTone";

const MODES: PermissionMode[] = ["plan", "ask", "auto", "allow"];

/**
 * The look the top effort rung wears wherever it appears.
 *
 * Violet rather than the accent colour, so it reads as its own thing next to
 * the model and mode buttons instead of as another selected control. The token
 * itself lives in the theme; this is only where it is applied.
 */
const ULTRA_TRIGGER_CLASS =
  "border border-[var(--ultra-edge)] bg-[var(--ultra-wash)] text-[var(--ultra-ink)] " +
  "hover:bg-[var(--ultra-wash-strong)] hover:text-[var(--ultra-ink)]";
/// Armed styling for the fast-mode toggle. It borrows the ultra treatment so
/// the two premium controls read as the same kind of thing in the footer: both
/// say "this turn costs more than the default".
const FAST_TRIGGER_CLASS = ULTRA_TRIGGER_CLASS;
const STEERING_UNAVAILABLE =
  "Steer is not available for this engine. Choose Queue to send this after the response.";

function appendComposerPrompt(current: string, prompt: string): string {
  const existing = current.trimEnd();
  const offered = prompt.trim();
  if (!existing) return offered;
  if (existing === offered || existing.endsWith(`\n\n${offered}`)) {
    return current;
  }
  return `${existing}\n\n${offered}`;
}

export function PermissionModePicker({
  value,
  availableModes = MODES,
  unavailableReason,
  disabled,
  pending,
  onChange,
  scopeKey = "code-create",
}: {
  value: PermissionMode;
  availableModes?: readonly PermissionMode[];
  unavailableReason?: string;
  disabled?: boolean;
  pending?: boolean;
  onChange?: (mode: PermissionMode) => void;
  scopeKey?: string;
}) {
  const locked = !onChange;
  const menu = (
    <PermissionModeMenu
      scopeKey={scopeKey}
      value={value}
      clampDisplay={false}
      disabled={locked || Boolean(unavailableReason) || disabled || pending}
      availableModes={availableModes}
      onChange={async (mode: PermissionMode) => {
        if (!availableModes.includes(mode)) {
          throw new Error(PERMISSION_MODE_UNAVAILABLE_REASON);
        }
        onChange?.(mode);
      }}
    />
  );
  let control: ReactNode;
  if (pending) {
    control = (
      <WithTooltip label="Saving session settings">
        <span className="inline-flex" aria-busy="true">
          {menu}
        </span>
      </WithTooltip>
    );
  } else if (unavailableReason) {
    control = (
      <WithTooltip label={unavailableReason}>
        <span className="inline-flex">{menu}</span>
      </WithTooltip>
    );
  } else if (!locked) {
    control = menu;
  } else {
    // Disabled buttons drop pointer events, so the tooltip has to sit on a
    // wrapper the reader can still hover and focus.
    control = (
      <WithTooltip label={SESSION_PERMISSION_MODE_LOCKED}>
        <span className="inline-flex">{menu}</span>
      </WithTooltip>
    );
  }
  return <span className="flex min-w-0 flex-col items-end">{control}</span>;
}

/** Rail entry that lifts the vendor filter off a mixed catalog. */
const ALL_MODELS = "all";

/** The mark for one picker row: vendor, then open-model family, then the engine. */
function CodeModelMark({
  harness,
  option,
  className,
}: {
  harness: HarnessKind;
  option: CodeModelOption;
  className?: string;
}) {
  const vendor = codeModelVendor(option);
  if (vendor || familyForModelId(option.id)) {
    return (
      <ProviderIcon
        provider={vendor ?? "model_gateway"}
        modelId={option.id}
        className={className}
      />
    );
  }
  const Icon = HARNESS_ICONS[harness];
  return <Icon className={className} />;
}

/**
 * Per-session model selector for the code composer.
 *
 * A vendor rail narrows the list. Search narrows whatever that rail is
 * showing: All crosses vendors, and a vendor tab stays on that vendor.
 * Choosing a tab clears the query, so the tab's models are what you see.
 * The rows are confined to what the engine can drive.
 */
export function HarnessModelMenu({
  harness,
  options,
  value,
  onChange,
  disabled,
  loading = false,
  variant = "composer",
  open: controlledOpen,
  onOpenChange,
}: {
  harness: HarnessKind;
  options: readonly CodeModelOption[];
  value?: string;
  onChange?: (model: string) => void;
  disabled?: boolean;
  loading?: boolean;
  /** Composer sits above the draft; field fills a form row. */
  variant?: "composer" | "field";
  /** Open the menu from outside — a surface's keyboard shortcut. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  const current =
    options.find((option) => option.id === value) ??
    options.find((option) => option.default) ??
    options[0];
  const [uncontrolledOpen, setUncontrolledOpen] = useState(false);
  const open = controlledOpen ?? uncontrolledOpen;
  const setOpen = (next: boolean) => {
    if (controlledOpen === undefined) setUncontrolledOpen(next);
    onOpenChange?.(next);
  };
  const [query, setQuery] = useState("");
  const searchInput = useRef<HTMLInputElement>(null);
  const currentRow = useRef<HTMLDivElement>(null);
  const groups = useMemo(() => groupCodeModelOptions(options), [options]);
  const currentGroupId =
    groups.find((group) =>
      group.options.some((option) => option.id === current?.id),
    )?.id ?? null;
  // A vendor-neutral engine lists a mixed catalog, and opening on the current
  // model's vendor showed one block of it — a menu with a single row, next to
  // a rail of unlabelled marks. Such a catalog opens on everything instead,
  // and the rail narrows from there.
  const mixed = groups.length > 1;
  const openingGroupId = mixed ? ALL_MODELS : currentGroupId;
  const [activeGroupId, setActiveGroupId] = useState<string | null>(
    openingGroupId,
  );
  const activeGroup =
    activeGroupId === ALL_MODELS && mixed
      ? null
      : (groups.find((group) => group.id === activeGroupId) ??
        groups[0] ??
        null);
  const showingAll = activeGroup === null && mixed;
  const searching = query.trim().length > 0;
  // Search stays inside the rail selection. All is the cross-vendor view;
  // a vendor tab must keep narrowing even while a query is typed.
  const scopedGroups = showingAll ? groups : activeGroup ? [activeGroup] : [];
  const visible = useMemo(
    () =>
      searching
        ? matchingCodeModels(scopedGroups, query)
        : scopedGroups.flatMap((group) => group.options),
    [scopedGroups, searching, query],
  );
  function selectGroup(id: string) {
    setActiveGroupId(id);
    setQuery("");
  }
  const locked = disabled || !onChange;
  useEffect(() => {
    // Opening resets the search and rail — whether the trigger or a caller's
    // keyboard chord opened it — and puts the session's row on screen: a
    // mixed catalog opens on the whole list, which runs past the fold.
    if (!open) return;
    setQuery("");
    setActiveGroupId(openingGroupId);
    const frame = requestAnimationFrame(() =>
      currentRow.current?.scrollIntoView({ block: "nearest" }),
    );
    return () => cancelAnimationFrame(frame);
    // `openingGroupId` moving mid-open (a catalog answer landing) must not
    // yank the rail out from under the reader.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);
  if (!current) {
    if (variant !== "field" && !loading) return null;
    const label = loading ? "Loading models…" : "Default model";
    return (
      <Button
        type="button"
        variant={variant === "field" ? "outline" : "ghost"}
        className={
          variant === "field"
            ? "h-10 w-full justify-between px-3 font-normal"
            : "h-8 max-w-56 min-w-0 shrink gap-2"
        }
        disabled
        aria-label={loading ? "Loading models" : "Model: Default"}
      >
        <span className="text-muted-foreground truncate">{label}</span>
        <ChevronDown className="size-4 opacity-50" />
      </Button>
    );
  }

  function modelRow(option: CodeModelOption, index: number) {
    const selected = option.id === current?.id;
    return (
      <DropdownMenuItem
        key={`${option.source}:${option.id}`}
        ref={selected ? currentRow : undefined}
        onSelect={() => onChange?.(option.id)}
        className="flex items-center gap-2"
      >
        <CodeModelMark
          harness={harness}
          option={option}
          className="size-4 shrink-0"
        />
        <span className="min-w-0 flex-1 truncate text-sm" title={option.label}>
          {option.label}
        </span>
        {index < 9 && (
          <span className="text-muted-foreground rounded-md border px-1.5 py-0.5 font-mono text-2xs">
            ⌘{index + 1}
          </span>
        )}
        {selected && <Check className="ml-auto size-4 shrink-0" />}
      </DropdownMenuItem>
    );
  }

  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant={variant === "field" ? "outline" : "ghost"}
          className={
            variant === "field"
              ? "h-10 w-full justify-between px-3 font-normal"
              : "h-8 max-w-56 min-w-0 shrink gap-2"
          }
          disabled={locked}
          aria-label={`Model: ${current.label}`}
          title={
            locked && !onChange
              ? `Model: ${current.label} (set when this session started)`
              : `Model: ${current.label}`
          }
        >
          <span className="flex min-w-0 items-center gap-2">
            <CodeModelMark
              harness={harness}
              option={current}
              className="size-4 shrink-0"
            />
            <span className="truncate">{current.label}</span>
          </span>
          <ChevronDown className="size-4 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        side={variant === "field" ? "bottom" : "top"}
        collisionPadding={12}
        // z-[60] matches the select primitive: this menu also opens inside
        // dialogs, whose overlay sits at z-50.
        className="model-menu-content z-[60] w-80 p-0"
        onKeyDownCapture={(event) => {
          if (
            (event.metaKey || event.ctrlKey) &&
            event.key >= "1" &&
            event.key <= "9"
          ) {
            const option = visible[Number(event.key) - 1];
            if (option) {
              event.preventDefault();
              onChange?.(option.id);
              setOpen(false);
            }
            return;
          }
          if (event.target === searchInput.current) return;
          if (event.metaKey || event.ctrlKey || event.altKey) return;
          if (event.key.length !== 1) return;
          event.preventDefault();
          setQuery((currentQuery) => currentQuery + event.key);
          requestAnimationFrame(() => searchInput.current?.focus());
        }}
      >
        {groups.length > 0 && (
          <div
            className="flex min-h-0 w-11 shrink-0 flex-col gap-1 overflow-y-auto border-r border-border bg-muted/30 p-1"
            role="tablist"
            aria-label="Vendors"
          >
            {mixed && (
              <WithTooltip label="All models" side="right">
                <button
                  type="button"
                  role="tab"
                  aria-selected={showingAll}
                  aria-label="All models"
                  className={cn(
                    "relative flex size-9 items-center justify-center rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    showingAll ? "bg-accent" : "hover:bg-accent/60",
                  )}
                  onClick={() => selectGroup(ALL_MODELS)}
                >
                  <List className="size-4" />
                  {showingAll && (
                    <span
                      aria-hidden
                      className="absolute -right-1 top-1/2 h-4 w-0.5 -translate-y-1/2 rounded-l-full bg-primary"
                    />
                  )}
                </button>
              </WithTooltip>
            )}
            {groups.map((group) => {
              const selected = activeGroup?.id === group.id;
              return (
                <WithTooltip key={group.id} label={group.label} side="right">
                  <button
                    type="button"
                    role="tab"
                    aria-selected={selected}
                    aria-label={group.label}
                    className={cn(
                      "relative flex size-9 items-center justify-center rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring",
                      selected ? "bg-accent" : "hover:bg-accent/60",
                    )}
                    onClick={() => selectGroup(group.id)}
                  >
                    <ProviderIcon
                      provider={group.iconProvider}
                      modelId={group.iconModelId}
                      className="size-4"
                    />
                    {selected && (
                      <span
                        aria-hidden
                        className="absolute -right-1 top-1/2 h-4 w-0.5 -translate-y-1/2 rounded-l-full bg-primary"
                      />
                    )}
                  </button>
                </WithTooltip>
              );
            })}
          </div>
        )}

        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <div className="border-border border-b p-1.5">
            <label className="bg-muted/40 focus-within:ring-ring flex h-8 items-center gap-2 rounded-md px-2 focus-within:ring-2">
              <Search className="text-muted-foreground size-3.5 shrink-0" />
              <input
                ref={searchInput}
                type="search"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                onKeyDown={(event) => event.stopPropagation()}
                placeholder="Search models"
                aria-label="Search models"
                className="placeholder:text-muted-foreground min-w-0 flex-1 bg-transparent text-sm outline-none"
              />
            </label>
          </div>
          <div className="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto p-1">
            {visible.map((option, index) => modelRow(option, index))}
            {visible.length === 0 && (
              <p className="text-muted-foreground px-2 py-3 text-sm">
                No models match that search.
              </p>
            )}
          </div>
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/**
 * Whether a level is the top rung this engine and model offer.
 *
 * The top rung is not the same level everywhere — Codex reaches `ultra`,
 * Claude Code's own picker calls its top ultracode, grok stops at `xhigh` —
 * so the treatment below keys on position in the offered ladder rather than
 * on one hard-coded name.
 */
function isTopEffort(
  levels: readonly ReasoningEffort[],
  value: ReasoningEffort | null,
): boolean {
  const options = reasoningEffortOptions(levels);
  const top = options[options.length - 1]?.value;
  return value !== null && top !== undefined && value === top;
}

/**
 * Composer-chrome effort picker. Chat's `ReasoningEffortSubMenu` is a tools
 * submenu; code wants the same levels sitting next to the model button.
 *
 * The top rung is styled apart from the rest. It is the one level that changes
 * what a turn costs and how long it runs by more than a step, so the control
 * says so while it is selected rather than reading like any other choice.
 */
/**
 * Fast mode as a single footer toggle.
 *
 * A toggle rather than a menu because there are two states and no default to
 * fall back to — unlike effort, where "the engine's own level" is a real third
 * choice. Renders nothing when the selected model cannot serve the tier, which
 * is the same rule an empty effort ladder follows: a control that would arm a
 * premium the model ignores is worse than no control.
 *
 * The label names the cost, not the speed. "Faster" alone reads as free.
 */
export function FastModeToggle({
  available,
  value,
  disabled,
  pending,
  onChange,
}: {
  available: boolean;
  value: boolean;
  disabled?: boolean;
  pending?: boolean;
  onChange: (fastMode: boolean) => void;
}) {
  if (!available) return null;
  const label = value ? "Fast mode on" : "Fast mode off";
  return (
    <Button
      type="button"
      variant="ghost"
      role="switch"
      aria-checked={value}
      className={cn("h-8 gap-2 px-2", value && FAST_TRIGGER_CLASS)}
      disabled={disabled}
      aria-busy={pending}
      aria-label={label}
      title={
        pending
          ? "Saving session settings"
          : value
            ? "Fast mode on: faster output, higher price per token"
            : "Fast mode off: the engine's usual speed and price"
      }
      data-fast={value ? "on" : undefined}
      onClick={() => onChange(!value)}
    >
      {pending ? (
        <Spinner className="size-4" />
      ) : (
        <Zap className={cn("size-4", !value && "opacity-50")} />
      )}
    </Button>
  );
}

export function ReasoningEffortMenu({
  levels,
  value,
  disabled,
  pending,
  onChange,
}: {
  levels: readonly ReasoningEffort[];
  value: ReasoningEffort | null;
  disabled?: boolean;
  pending?: boolean;
  onChange: (effort: ReasoningEffort | null) => void;
}) {
  const options = reasoningEffortOptions(levels);
  if (options.length === 0) return null;
  const isDefault = value === null;
  const label = isDefault
    ? "Default"
    : (reasoningEffortOptions([value])[0]?.label ?? "Default");
  const topSelected = isTopEffort(levels, value);
  const topValue = options[options.length - 1]?.value;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          className={cn(
            "h-8 max-w-40 gap-2",
            topSelected && ULTRA_TRIGGER_CLASS,
          )}
          disabled={disabled}
          aria-busy={pending}
          aria-label={`Reasoning: ${label}`}
          title={pending ? "Saving session settings" : `Reasoning: ${label}`}
          data-ultra={topSelected ? "on" : undefined}
        >
          {pending ? (
            <Spinner className="size-4" />
          ) : topSelected ? (
            <Sparkles className="size-4" />
          ) : (
            <Gauge className="size-4" />
          )}
          <span className="truncate">{label}</span>
          <ChevronDown className="size-4 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="top" className="w-48">
        <DropdownMenuItem
          disabled={disabled}
          onSelect={() => {
            if (!isDefault) onChange(null);
          }}
          className="flex items-center gap-2"
        >
          <span className="text-sm">Default</span>
          {isDefault && <Check className="ml-auto size-4" />}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        {options.map((option) => {
          const selected = !isDefault && value === option.value;
          const top = option.value === topValue;
          return (
            <DropdownMenuItem
              key={option.value}
              disabled={disabled}
              onSelect={() => {
                if (!selected) onChange(option.value);
              }}
              className={cn(
                "flex items-center gap-2",
                top && "text-[var(--ultra-ink)]",
              )}
            >
              {top && <Sparkles className="size-3.5" />}
              <span className="text-sm">{option.label}</span>
              {selected && <Check className="ml-auto size-4" />}
            </DropdownMenuItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

export function CodeComposer({
  disabled,
  running,
  permissionMode,
  availableModes = MODES,
  unavailableReason,
  harness,
  model,
  modelOptions,
  modelLoading = false,
  harnessMenu,
  footerNote,
  promptScope,
  sessionId,
  startSession,
  history,
  reasoningEffort = null,
  engineEfforts = [],
  fastMode = false,
  settingsPending = false,
  onModelChange,
  onModeChange,
  onEffortChange,
  onFastModeChange,
  contextUsage,
  slashCommands,
  searchPaths,
  workspaceFiles,
  reviewWorkspaceId,
  onSend,
  onSteer,
  onInterrupt,
}: {
  disabled?: boolean;
  running: boolean;
  permissionMode: PermissionMode;
  availableModes?: readonly PermissionMode[];
  /** Why no permission mode can start this session. */
  unavailableReason?: string;
  harness?: HarnessKind;
  model?: string;
  modelOptions?: readonly CodeModelOption[];
  modelLoading?: boolean;
  /** Engine picker shown before the model control when starting a session. */
  harnessMenu?: ReactNode;
  /** Setup status shown inside the composer after its controls. */
  footerNote?: ReactNode;
  /** Workspace identity used to route header actions to the matching composer. */
  promptScope?: string;
  /** The session this composer sends to. Absent on the start surface. */
  sessionId?: string;
  /**
   * The start surface's way to create the session its first message goes
   * to. The send creates it, hands this draft to that session's composer, and
   * finishes there through the same path as any other message.
   */
  startSession?: () => Promise<string>;
  /** Prior user prompts, newest first, for Up/Down recall. */
  history?: readonly string[];
  /** The session's stored level. `null` is the engine's own default. */
  reasoningEffort?: ReasoningEffort | null;
  /** Whether the session is armed for fast mode. */
  fastMode?: boolean;
  /** Whether a permission, reasoning, or fast-mode write is active. */
  settingsPending?: boolean;
  /**
   * The engine's own ladder, used for a model row that states none of its
   * own — a gateway catalog row, or a model the engine no longer lists.
   */
  engineEfforts?: readonly ReasoningEffort[];
  onModelChange?: (model: string) => void;
  onModeChange?: (mode: PermissionMode) => void;
  /** Absent hides the effort control, as an empty ladder does. */
  onEffortChange?: (effort: ReasoningEffort | null) => void;
  /** Absent hides the fast-mode toggle, as an unsupporting model does. */
  onFastModeChange?: (fastMode: boolean) => void;
  /** Same meter as chat: the last turn's reading in the send cluster. */
  contextUsage?: ContextUsageReading | null;
  /**
   * Engine-discovered slash commands. Empty or absent hides the `/` popup;
   * free-typed `/` text still submits verbatim.
   */
  slashCommands?: readonly { name: string; description: string }[];
  /** Name-matched workspace paths for `@` completion. */
  searchPaths?: (query: string) => Promise<readonly string[]>;
  /**
   * Files already in the worktree, shown as chips and named after the
   * message. A fork's transcript arrives this way.
   */
  workspaceFiles?: ComposerWorkspaceFiles;
  /**
   * The workspace whose diff comments go with the next message. The
   * composer counts them, and the send carries them.
   */
  reviewWorkspaceId?: string;
  /**
   * Post one message to `sessionId`, adding what the session view knows: a
   * model or effort change, following the transcript. Absent, the message is
   * posted as it is, which is what the start surface does with the session
   * its send just created.
   */
  onSend?: (
    message: string,
    attachments?: readonly CodeTurnImage[],
  ) => Promise<CodeTurnSubmission | void> | void;
  /**
   * Redirect the in-flight turn. Absent when the harness cannot steer. The
   * composer refuses Redirect in that state; Queue is only used when the user
   * explicitly selected Queue.
   */
  onSteer?: (message: string) => Promise<void>;
  onInterrupt: () => Promise<void> | void;
}) {
  const { client } = useApp();
  const post = async (
    message: string,
    attachments: readonly CodeTurnImage[] | undefined,
    target: string,
  ) => {
    if (!onSend) {
      return sendCodeTurn({ client, sessionId: target, message, attachments });
    }
    return attachments ? onSend(message, attachments) : onSend(message);
  };
  const composerPromptScope = promptScope ?? sessionId ?? "code";
  // The draft, pasted text, and images live in the same store chat uses,
  // keyed by session so one draft cannot leak into another. Settings unmounts
  // this composer, and a send outlives the start surface that began it.
  const draftKey = sessionId ?? composerPromptScope;
  const draft = useComposerDraft(draftKey);
  const pastedTexts = useComposerAttachments(draftKey).pastedTexts;
  const { sending, notice } = useCodeComposerSendStatus(draftKey);
  const [selectedModel, setSelectedModel] = useState(model ?? "");
  // Optimistic: the picker moves on click and the session row catches up when
  // the route answers. A refusal is surfaced by the caller, which owns the
  // request and re-renders this from the session it holds.
  const [selectedEffort, setSelectedEffort] = useState<ReasoningEffort | null>(
    reasoningEffort,
  );
  const [selectedFastMode, setSelectedFastMode] = useState(fastMode);
  const [steerPending, setSteerPending] = useState(false);
  const [steerError, setSteerError] = useState<string | null>(null);
  const [steerStatus, setSteerStatus] = useState<string | null>(null);
  const draftRef = useRef(draft);
  const setDraft = useCallback(
    (update: string | ((current: string) => string)) => {
      const current = useComposerDrafts.getState().drafts[draftKey] ?? "";
      const next = typeof update === "function" ? update(current) : update;
      draftRef.current = next;
      useComposerDrafts.getState().setDraft(draftKey, next);
    },
    [draftKey],
  );
  const steerRequestRef = useRef(0);
  const imageInputRef = useRef<HTMLInputElement>(null);
  // With no session yet there is nothing to publish to, so attached images
  // are held until the send creates one.
  const images = useImageAttachments(
    client,
    draftKey,
    sessionId ? async () => sessionId : undefined,
    "code",
    { hold: !sessionId },
  );
  const [pathItems, setPathItems] = useState<string[]>([]);
  const searchPathsRef = useRef(searchPaths);
  searchPathsRef.current = searchPaths;
  const selectedOption =
    modelOptions?.find((option) => option.id === selectedModel) ??
    modelOptions?.find((option) => option.default) ??
    modelOptions?.[0];
  const effortLevels = effortLadder(selectedOption, engineEfforts);
  // Read off the selected row, so switching to a model without the tier hides
  // the control rather than leaving a stale armed toggle behind.
  const fastModeAvailable = selectedOption?.fast_mode ?? false;

  const onPathQueryChange = useCallback((query: string | null) => {
    if (query === null || !searchPathsRef.current) {
      setPathItems([]);
      return;
    }
    void searchPathsRef.current(query).then(
      (paths) => setPathItems([...paths]),
      () => setPathItems([]),
    );
  }, []);

  const pathMentions = searchPaths
    ? { items: pathItems, onQueryChange: onPathQueryChange }
    : undefined;
  const slash =
    slashCommands && slashCommands.length > 0
      ? {
          options: slashCommands.map((command) => ({
            kind: "prompt" as const,
            name: command.name,
            label: `/${command.name}`,
            description: command.description,
          })),
          invoked: [],
          onInvoke: () => undefined,
          onRemove: () => undefined,
          loadPromptBody: async (name: string) => `/${name}`,
        }
      : undefined;

  const pendingPrompt = useCodeUiStore((state) => state.pendingComposerPrompt);
  const { confirm, dialog: confirmDialog } = useConfirm();
  const pendingReview = usePendingReview(reviewWorkspaceId);
  const summary = reviewSummary(pendingReview.comments, pendingReview.sending);
  // While a turn runs, comments wait for the next turn unless the reader adds
  // them to the steer or follow-up being written. Each turn starts with them
  // held back.
  const [addReviewNow, setAddReviewNow] = useState(false);
  useEffect(() => {
    setAddReviewNow(false);
  }, [running]);
  const reviewGoes = !running || addReviewNow;
  const reviewComments = useMemo<ComposerReviewComments | undefined>(() => {
    if (!reviewWorkspaceId || summary.count === 0) return undefined;
    return {
      count: summary.count,
      files: summary.files,
      included: reviewGoes,
      ...(running ? { onIncludedChange: setAddReviewNow } : {}),
      onRemove: () => {
        void confirm({
          title:
            summary.count === 1
              ? "Delete your review comment?"
              : `Delete ${summary.count} review comments?`,
          description:
            "They come off the diff and do not go to the agent. Their text is lost.",
          confirmLabel: "Delete",
          destructive: true,
        }).then((confirmed) => {
          if (confirmed) {
            usePendingReviewStore.getState().clear(reviewWorkspaceId);
          }
        });
      },
    };
  }, [
    reviewWorkspaceId,
    summary.count,
    summary.files,
    reviewGoes,
    running,
    confirm,
  ]);

  useEffect(() => {
    draftRef.current = draft;
  }, [draft]);

  function setPastedTexts(items: PastedTextAttachment[]) {
    useComposerDrafts.getState().setPastedTexts(draftKey, items);
  }

  useEffect(() => {
    steerRequestRef.current += 1;
    setSteerPending(false);
    setSteerError(null);
    setSteerStatus(null);
  }, [sessionId]);

  useEffect(() => {
    if (!pendingPrompt || pendingPrompt.scope !== composerPromptScope) return;
    const request = useCodeUiStore
      .getState()
      .takeComposerPrompt(composerPromptScope);
    if (!request) return;
    if (request.submit) {
      void submitOfferedPrompt(request.text);
      return;
    }
    setDraft((current) => appendComposerPrompt(current, request.text));
    window.requestAnimationFrame(() => {
      document
        .querySelector<HTMLTextAreaElement>("[data-composer-input]")
        ?.focus();
    });
  }, [composerPromptScope, pendingPrompt, sessionId]);

  useEffect(() => {
    if (model) setSelectedModel(model);
  }, [model]);

  useEffect(() => {
    setSelectedEffort(reasoningEffort);
  }, [reasoningEffort, sessionId]);

  // The toggle moves optimistically on click, so it has to follow the session
  // back down when the route refuses or answers with something else. Without
  // this the control keeps the value the click assumed and the spend switch
  // reads wrong until remount.
  useEffect(() => {
    setSelectedFastMode(fastMode);
  }, [fastMode, sessionId]);

  async function submit() {
    if (disabled) return;
    const session = sessionId ?? startSession;
    if (!session) return;
    // One send path for every code message. On the start surface it creates
    // the session first and hands this draft to that session's composer.
    const carriesReview = reviewGoes && reviewWorkspaceId !== undefined;
    const sent = await sendCodeComposer({
      client,
      key: draftKey,
      session,
      workspaceFiles: workspaceFiles?.items,
      reviewWorkspaceId: carriesReview ? reviewWorkspaceId : undefined,
      send: async (target, message, attachments) =>
        post(message, attachments, target),
    });
    if (sent && carriesReview) setAddReviewNow(false);
  }

  async function submitOfferedPrompt(text: string) {
    const message = text.trim();
    if (!message) return;
    if (disabled || !(sessionId ?? startSession)) {
      setDraft((current) => appendComposerPrompt(current, message));
      useCodeUiStore.getState().finishComposerAction(composerPromptScope);
      window.requestAnimationFrame(() => {
        document
          .querySelector<HTMLTextAreaElement>("[data-composer-input]")
          ?.focus();
      });
      return;
    }
    setCodeComposerNotice(draftKey, null);
    try {
      if (!sessionId && startSession) {
        await startSessionWithAction(message, startSession);
        return;
      }
      // Ran or queued, the prompt is on its way; the queue tray shows a
      // parked row.
      await post(message, undefined, sessionId!);
    } catch (err) {
      setCodeComposerNotice(draftKey, codeSendFailure(err));
      setDraft((current) => appendComposerPrompt(current, message));
      window.requestAnimationFrame(() => {
        document
          .querySelector<HTMLTextAreaElement>("[data-composer-input]")
          ?.focus();
      });
    } finally {
      useCodeUiStore.getState().finishComposerAction(composerPromptScope);
    }
  }

  /**
   * A one-click action on a workspace with no agent yet starts one, with the
   * action as its first message. The action rides its own draft key, so a
   * draft the reader had typed here stays where it is.
   */
  async function startSessionWithAction(
    message: string,
    create: () => Promise<string>,
  ) {
    const actionKey = `${draftKey}#action`;
    seedCodeComposer(actionKey, message);
    const sent = await sendCodeComposer({
      client,
      key: actionKey,
      session: create,
      send: async (target, text, attachments) =>
        post(text, attachments, target),
    });
    // No session started: bring the action back where the reader can see it.
    if (!sent && useComposerDrafts.getState().drafts[actionKey]) {
      moveComposerDraft(actionKey, draftKey);
      useCodeComposerStatus.getState().move(actionKey, draftKey);
    }
  }

  async function steer() {
    const submittedDraft = draftRef.current;
    const submittedPastedTexts = pastedTexts;
    const typed = messageWithPastedText(submittedDraft, submittedPastedTexts);
    // A steer carries the comments only when the reader added them to it.
    const reviewWorkspace =
      reviewGoes && reviewWorkspaceId !== undefined
        ? reviewWorkspaceId
        : undefined;
    const reviewReady =
      reviewWorkspace !== undefined &&
      commentsReadyToSend(reviewWorkspace).length > 0;
    if ((!typed && !reviewReady) || disabled) return;
    if (!onSteer) {
      setSteerStatus(null);
      setSteerError(STEERING_UNAVAILABLE);
      setCodeComposerNotice(draftKey, null);
      return;
    }
    const review = reviewWorkspace
      ? usePendingReviewStore.getState().claim(reviewWorkspace)
      : [];
    const settleReview = (accepted: boolean) => {
      if (!reviewWorkspace || review.length === 0) return;
      usePendingReviewStore
        .getState()
        .finishSend(reviewWorkspace, review, accepted);
      if (accepted) setAddReviewNow(false);
    };
    const message = messageWithReviewComments(typed, review, {
      turnName: sessionId ? turnNamer(sessionId) : undefined,
    });
    const request = steerRequestRef.current + 1;
    steerRequestRef.current = request;
    setSteerPending(true);
    setSteerError(null);
    setSteerStatus("Steering…");
    setCodeComposerNotice(draftKey, null);
    try {
      await onSteer(message);
      settleReview(true);
      if (steerRequestRef.current !== request) return;
      const current = useComposerDrafts.getState();
      if (
        (current.drafts[draftKey] ?? "") === submittedDraft &&
        (current.attachments[draftKey]?.pastedTexts ?? []) ===
          submittedPastedTexts
      ) {
        draftRef.current = "";
        setDraft("");
        setPastedTexts([]);
      }
      setSteerStatus("Steer sent");
    } catch (err) {
      settleReview(false);
      if (steerRequestRef.current !== request) return;
      setSteerStatus(null);
      setSteerError(err instanceof Error ? err.message : "Could not steer");
    } finally {
      if (steerRequestRef.current === request) setSteerPending(false);
    }
  }

  return (
    <div className="relative shrink-0 px-[clamp(0.5rem,4%,5rem)] pb-2">
      {confirmDialog}
      <Composer
        activeTurnId={running ? "running" : null}
        busy={running}
        cancelError={null}
        cancelPending={false}
        disabled={Boolean(disabled || (sending && !running))}
        draft={draft}
        history={history}
        harnessMenu={harnessMenu}
        modelMenu={
          harness && ((modelOptions?.length ?? 0) > 0 || modelLoading) ? (
            <HarnessModelMenu
              harness={harness}
              options={modelOptions ?? []}
              value={selectedModel || undefined}
              loading={modelLoading}
              onChange={
                onModelChange
                  ? (next) => {
                      setSelectedModel(next);
                      onModelChange(next);
                    }
                  : undefined
              }
            />
          ) : undefined
        }
        fastModeToggle={
          onFastModeChange ? (
            <FastModeToggle
              available={fastModeAvailable}
              value={selectedFastMode}
              pending={settingsPending}
              onChange={(next) => {
                setSelectedFastMode(next);
                onFastModeChange(next);
              }}
            />
          ) : undefined
        }
        effortMenu={
          effortLevels.length > 0 && onEffortChange ? (
            <ReasoningEffortMenu
              levels={effortLevels}
              value={selectedEffort}
              pending={settingsPending}
              onChange={(next) => {
                setSelectedEffort(next);
                onEffortChange(next);
              }}
            />
          ) : undefined
        }
        permissionMenu={
          <PermissionModePicker
            value={permissionMode}
            availableModes={availableModes}
            unavailableReason={unavailableReason}
            pending={settingsPending}
            onChange={onModeChange}
            scopeKey={sessionId ?? "code-create"}
          />
        }
        contextUsage={contextUsage}
        pathMentions={pathMentions}
        slash={slash}
        images={{
          items: images.attachments,
          error: images.error,
          unsupportedModel: null,
          onAttachFiles: images.attachFiles,
          onRemove: images.remove,
          onRetry: images.retry,
        }}
        files={{
          items: [],
          attaching: false,
          onAttach: () => imageInputRef.current?.click(),
          onRemove: () => undefined,
        }}
        pastedTexts={{
          items: pastedTexts,
          onPaste: (text) =>
            setPastedTexts([
              ...(useComposerDrafts.getState().attachments[draftKey]
                ?.pastedTexts ?? []),
              { id: crypto.randomUUID(), text },
            ]),
          onRemove: (id) =>
            setPastedTexts(
              (
                useComposerDrafts.getState().attachments[draftKey]
                  ?.pastedTexts ?? []
              ).filter((item) => item.id !== id),
            ),
        }}
        workspaceFiles={workspaceFiles}
        reviewComments={reviewComments}
        onDraftChange={(value) => {
          draftRef.current = value;
          setSteerError(null);
          setSteerStatus(null);
          setDraft(value);
        }}
        onSend={submit}
        onSteer={steer}
        onQueue={submit}
        onStop={async () => {
          await onInterrupt();
        }}
        resetKey={sessionId ?? "code"}
        steerError={steerError}
        steerPending={steerPending}
        steerStatus={steerStatus}
        footerNote={
          <>
            {unavailableReason && (
              <p className="text-muted-foreground text-xs">
                {unavailableReason}
              </p>
            )}
            {footerNote}
          </>
        }
      />
      <input
        ref={imageInputRef}
        type="file"
        accept={IMAGE_MEDIA_TYPES.join(",")}
        multiple
        className="hidden"
        aria-label="Attach images"
        onChange={(event) => {
          const files = [...(event.target.files ?? [])];
          event.target.value = "";
          images.attachFiles(files);
        }}
      />
      {notice && (
        <p
          role="alert"
          className={cn(STATUS_TEXT.critical, "mx-auto max-w-3xl pt-1 text-xs")}
        >
          {notice}
        </p>
      )}
    </div>
  );
}
