import { useCallback, useMemo, useRef, useState, useEffect } from "react";
import { Check, ChevronDown, Gauge, Search, Sparkles } from "lucide-react";

import type {
  CodePermissionMode,
  HarnessKind,
  PermissionMode,
  ReasoningEffort,
} from "../api/types";
import { useApp } from "@/AppContext";
import { HttpError } from "../api/client";
import { Composer, type ComposerWorkspaceFiles } from "../Composer";
import { messageWithWorkspaceFiles } from "./fork";
import { IMAGE_MEDIA_TYPES } from "../ImageAttachments";
import { useImageAttachments } from "../useImageAttachments";
import { reasoningEffortOptions } from "../ModelMenu";
import { familyForModelId } from "../modelFamilies";
import { PermissionModeMenu } from "../PermissionModeMenu";
import {
  ClaudeIcon,
  OpenAIIcon,
  OpenCodeIcon,
  ProviderIcon,
  XaiIcon,
} from "../ProviderIcons";
import { Button } from "@/components/ui/button";
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
import { useCodeUiStore } from "./CodeUiStore";
import {
  codeModelVendor,
  effortLadder,
  groupCodeModelOptions,
  type CodeModelOption,
  PERMISSION_MODE_UNAVAILABLE_REASON,
  SESSION_PERMISSION_MODE_LOCKED,
} from "./labels";

const MODES: CodePermissionMode[] = ["plan", "ask", "auto", "allow"];

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
const STEERING_UNAVAILABLE =
  "Redirect isn’t available for this harness. Choose Queue to send this after the response.";

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
  onChange,
  scopeKey = "code-create",
}: {
  value: CodePermissionMode;
  availableModes?: readonly CodePermissionMode[];
  unavailableReason?: string;
  onChange?: (mode: CodePermissionMode) => void;
  scopeKey?: string;
}) {
  const locked = !onChange;
  const menu = (
    <PermissionModeMenu
      scopeKey={scopeKey}
      value={value}
      disabled={locked}
      onChange={async (mode: PermissionMode) => {
        if (!availableModes.includes(mode)) {
          throw new Error(PERMISSION_MODE_UNAVAILABLE_REASON);
        }
        onChange?.(mode);
      }}
    />
  );
  if (!locked) return menu;
  // Disabled buttons drop pointer events, so the tooltip has to sit on a
  // wrapper the reader can still hover and focus.
  return (
    <WithTooltip label={SESSION_PERMISSION_MODE_LOCKED}>
      <span className="inline-flex">{menu}</span>
    </WithTooltip>
  );
}

const HARNESS_ICONS: Record<HarnessKind, typeof ClaudeIcon> = {
  claude_code: ClaudeIcon,
  codex: OpenAIIcon,
  opencode: OpenCodeIcon,
  grok: XaiIcon,
};

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
 * Mirrors the chat picker: a vendor rail on the left, search on top, one
 * row per model. The rows are confined to what the engine can drive, so a
 * Claude Code session only ever offers Claude models while vendor-neutral
 * engines group a mixed catalog by vendor.
 */
export function HarnessModelMenu({
  harness,
  options,
  value,
  onChange,
  disabled,
  loading = false,
  variant = "composer",
}: {
  harness: HarnessKind;
  options: readonly CodeModelOption[];
  value?: string;
  onChange?: (model: string) => void;
  disabled?: boolean;
  loading?: boolean;
  /** Composer sits above the draft; field fills a form row. */
  variant?: "composer" | "field";
}) {
  const current =
    options.find((option) => option.id === value) ??
    options.find((option) => option.default) ??
    options[0];
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const searchInput = useRef<HTMLInputElement>(null);
  const groups = useMemo(() => groupCodeModelOptions(options), [options]);
  const currentGroupId =
    groups.find((group) =>
      group.options.some((option) => option.id === current?.id),
    )?.id ?? null;
  const [activeGroupId, setActiveGroupId] = useState<string | null>(
    currentGroupId,
  );
  const activeGroup =
    groups.find((group) => group.id === activeGroupId) ?? groups[0] ?? null;
  const searching = query.trim().length > 0;
  const matches = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return [];
    return groups
      .flatMap((group) => group.options)
      .filter(
        (option) =>
          option.label.toLowerCase().includes(needle) ||
          option.id.toLowerCase().includes(needle) ||
          option.source.toLowerCase().includes(needle),
      );
  }, [groups, query]);
  const visible = searching ? matches : (activeGroup?.options ?? []);
  const locked = disabled || !onChange;
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
            : "h-8 max-w-56 gap-2"
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
    <DropdownMenu
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) {
          setQuery("");
          setActiveGroupId(currentGroupId);
        }
      }}
    >
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant={variant === "field" ? "outline" : "ghost"}
          className={
            variant === "field"
              ? "h-10 w-full justify-between px-3 font-normal"
              : "h-8 max-w-56 gap-2"
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
        className={cn(
          "model-menu-content w-80 p-0",
          variant === "field" && "z-[60]",
        )}
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
                    onClick={() => setActiveGroupId(group.id)}
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
export function ReasoningEffortMenu({
  levels,
  value,
  disabled,
  onChange,
}: {
  levels: readonly ReasoningEffort[];
  value: ReasoningEffort | null;
  disabled?: boolean;
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
          aria-label={`Reasoning: ${label}`}
          title={`Reasoning: ${label}`}
          data-ultra={topSelected ? "on" : undefined}
        >
          {topSelected ? (
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
  harness,
  model,
  modelOptions,
  modelLoading = false,
  promptScope,
  sessionId,
  history,
  queued = false,
  lastTurnBeganId,
  reasoningEffort = null,
  engineEfforts = [],
  onModelChange,
  onModeChange,
  onEffortChange,
  contextUsage,
  slashCommands,
  searchPaths,
  imageInput = false,
  workspaceFiles,
  onSend,
  onSteer,
  onInterrupt,
}: {
  disabled?: boolean;
  running: boolean;
  permissionMode: CodePermissionMode;
  availableModes?: readonly CodePermissionMode[];
  unavailableReason?: string;
  harness?: HarnessKind;
  model?: string;
  modelOptions?: readonly CodeModelOption[];
  modelLoading?: boolean;
  /** Workspace identity used to route header actions to the matching composer. */
  promptScope?: string;
  sessionId?: string;
  /** Prior user prompts, newest first, for Up/Down recall. */
  history?: readonly string[];
  /** A follow-up the pane is holding until the next turn begins. */
  queued?: boolean;
  /**
   * The turn the journal last announced. A change drops the queued pill once
   * the parked follow-up starts.
   */
  lastTurnBeganId?: string | null;
  /** The session's stored level. `null` is the engine's own default. */
  reasoningEffort?: ReasoningEffort | null;
  /**
   * The engine's own ladder, used for a model row that states none of its
   * own — a gateway catalog row, or a model the engine no longer lists.
   */
  engineEfforts?: readonly ReasoningEffort[];
  onModelChange?: (model: string) => void;
  onModeChange?: (mode: CodePermissionMode) => void;
  /** Absent hides the effort control, as an empty ladder does. */
  onEffortChange?: (effort: ReasoningEffort | null) => void;
  /** Same meter as chat: the last turn's reading in the send cluster. */
  contextUsage?: ContextUsageReading | null;
  /**
   * Engine-discovered slash commands. Empty or absent hides the `/` popup;
   * free-typed `/` text still submits verbatim.
   */
  slashCommands?: readonly { name: string; description: string }[];
  /** Name-matched workspace paths for `@` completion. */
  searchPaths?: (query: string) => Promise<readonly string[]>;
  /** The doctor said this engine consumes images on its input path. */
  imageInput?: boolean;
  /**
   * Files already in the worktree, shown as chips and named after the
   * message. A fork's transcript arrives this way.
   */
  workspaceFiles?: ComposerWorkspaceFiles;
  onSend: (
    message: string,
    attachments?: readonly { blob_id: string; media_type: string }[],
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
  const composerPromptScope = promptScope ?? sessionId ?? "code";
  const [draft, setDraft] = useState("");
  const [selectedModel, setSelectedModel] = useState(model ?? "");
  // Optimistic: the picker moves on click and the session row catches up when
  // the route answers. A refusal is surfaced by the caller, which owns the
  // request and re-renders this from the session it holds.
  const [selectedEffort, setSelectedEffort] = useState<ReasoningEffort | null>(
    reasoningEffort,
  );
  const [notice, setNotice] = useState<{ text: string } | null>(null);
  const [followUpQueued, setFollowUpQueued] = useState(false);
  const [steerPending, setSteerPending] = useState(false);
  const [steerError, setSteerError] = useState<string | null>(null);
  const [steerStatus, setSteerStatus] = useState<string | null>(null);
  const draftRef = useRef("");
  const steerRequestRef = useRef(0);
  const imageInputRef = useRef<HTMLInputElement>(null);
  const images = useImageAttachments(
    client,
    sessionId ?? "code-composer",
    sessionId ? async () => sessionId : undefined,
    "code",
  );
  const showQueued = queued || followUpQueued;
  const [pathItems, setPathItems] = useState<string[]>([]);
  const searchPathsRef = useRef(searchPaths);
  searchPathsRef.current = searchPaths;
  const selectedOption =
    modelOptions?.find((option) => option.id === selectedModel) ??
    modelOptions?.find((option) => option.default) ??
    modelOptions?.[0];
  const effortLevels = effortLadder(selectedOption, engineEfforts);

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

  useEffect(() => {
    draftRef.current = draft;
  }, [draft]);

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
  }, [composerPromptScope, pendingPrompt]);

  useEffect(() => {
    if (model) setSelectedModel(model);
  }, [model]);

  useEffect(() => {
    setSelectedEffort(reasoningEffort);
  }, [reasoningEffort, sessionId]);

  useEffect(() => {
    setFollowUpQueued(false);
  }, [lastTurnBeganId]);

  async function submit() {
    const typed = draft.trim();
    if (!typed || disabled) return;
    // The chips ride out with the message: the engine reads the paths from
    // its own working directory, so nothing is uploaded.
    const message = messageWithWorkspaceFiles(
      typed,
      workspaceFiles?.items ?? [],
    );
    const pending = images.attachments.filter(
      (item) => item.status === "queued" || item.status === "uploading",
    );
    if (pending.length > 0) {
      setNotice({ text: "Wait for images to finish attaching." });
      return;
    }
    if (images.attachments.some((item) => item.status === "failed")) {
      setNotice({ text: "Remove or retry the images that failed to attach." });
      return;
    }
    const attachments = images.attachments.flatMap((item) =>
      item.attachmentId
        ? [
            {
              blob_id: item.attachmentId,
              media_type: item.mediaType ?? "image/png",
            },
          ]
        : [],
    );
    const held = images.attachments;
    setDraft("");
    setNotice(null);
    // Chips leave with the draft, not after the server answers. Waiting made
    // a sent turn look like it had failed to take the images.
    images.clear();
    try {
      const outcome =
        attachments.length > 0
          ? await onSend(message, attachments)
          : await onSend(message);
      if (outcome && outcome.kind === "queued") {
        setFollowUpQueued(true);
      }
    } catch (err) {
      images.restore(held);
      setNotice({
        text:
          err instanceof HttpError && err.kind === "queue_full"
            ? "A follow-up is already queued. Wait for it to run, or interrupt this turn."
            : err instanceof Error
              ? err.message
              : "Could not send that turn",
      });
      setDraft((current) => (current.length === 0 ? message : current));
    }
  }

  async function submitOfferedPrompt(text: string) {
    const message = text.trim();
    if (!message) return;
    if (disabled) {
      setDraft((current) => appendComposerPrompt(current, message));
      useCodeUiStore.getState().finishComposerAction(composerPromptScope);
      window.requestAnimationFrame(() => {
        document
          .querySelector<HTMLTextAreaElement>("[data-composer-input]")
          ?.focus();
      });
      return;
    }
    setNotice(null);
    try {
      const outcome = await onSend(message);
      if (outcome && outcome.kind === "queued") {
        setFollowUpQueued(true);
      }
    } catch (err) {
      setNotice({
        text:
          err instanceof HttpError && err.kind === "queue_full"
            ? "A follow-up is already queued. Wait for it to run, or interrupt this turn."
            : err instanceof Error
              ? err.message
              : "Could not send that turn",
      });
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

  async function steer() {
    const submittedDraft = draftRef.current;
    const message = submittedDraft.trim();
    if (!message || disabled) return;
    if (!onSteer) {
      setSteerStatus(null);
      setSteerError(STEERING_UNAVAILABLE);
      setNotice(null);
      return;
    }
    const request = steerRequestRef.current + 1;
    steerRequestRef.current = request;
    setSteerPending(true);
    setSteerError(null);
    setSteerStatus("Sending guidance…");
    setNotice(null);
    try {
      await onSteer(message);
      if (steerRequestRef.current !== request) return;
      if (draftRef.current === submittedDraft) {
        draftRef.current = "";
        setDraft("");
      }
      setSteerStatus("Guidance sent");
    } catch (err) {
      if (steerRequestRef.current !== request) return;
      setSteerStatus(null);
      setSteerError(err instanceof Error ? err.message : "Could not steer");
    } finally {
      if (steerRequestRef.current === request) setSteerPending(false);
    }
  }

  return (
    <div className="relative shrink-0 px-[clamp(0.5rem,4%,5rem)] pb-2">
      {showQueued && (
        <p
          role="status"
          className="text-muted-foreground pointer-events-none absolute inset-x-0 bottom-full mx-auto mb-1 max-w-3xl text-center text-[11px] [animation:code-reveal_140ms_ease-out] motion-reduce:animate-none"
        >
          1 follow-up queued
        </p>
      )}
      <Composer
        activeTurnId={running ? "running" : null}
        busy={running}
        cancelError={null}
        cancelPending={false}
        disabled={Boolean(disabled)}
        draft={draft}
        history={history}
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
        effortMenu={
          effortLevels.length > 0 && onEffortChange ? (
            <ReasoningEffortMenu
              levels={effortLevels}
              value={selectedEffort}
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
            onChange={onModeChange}
            scopeKey={sessionId ?? "code-create"}
          />
        }
        contextUsage={contextUsage}
        pathMentions={pathMentions}
        slash={slash}
        images={
          imageInput
            ? {
                items: images.attachments,
                error: images.error,
                unsupportedModel: null,
                onAttachFiles: images.attachFiles,
                onRemove: images.remove,
                onRetry: images.retry,
              }
            : undefined
        }
        files={
          imageInput
            ? {
                items: [],
                attaching: false,
                onAttach: () => imageInputRef.current?.click(),
                onRemove: () => undefined,
              }
            : undefined
        }
        workspaceFiles={workspaceFiles}
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
      />
      {imageInput && (
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
      )}
      {notice && (
        <p
          role="alert"
          className="text-critical-foreground mx-auto max-w-3xl pt-1 text-[11px]"
        >
          {notice.text}
        </p>
      )}
    </div>
  );
}
