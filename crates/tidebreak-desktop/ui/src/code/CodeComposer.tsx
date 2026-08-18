import { useCallback, useMemo, useRef, useState, useEffect } from "react";
import { Check, ChevronDown, Search } from "lucide-react";

import type { CodePermissionMode, HarnessKind, PermissionMode } from "../api/types";
import { HttpError } from "../api/client";
import { Composer } from "../Composer";
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
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type { CodeTurnSubmission } from "./parsers";
import {
  codeModelVendor,
  groupCodeModelOptions,
  type CodeModelOption,
  PERMISSION_MODE_UNAVAILABLE_REASON,
  SESSION_PERMISSION_MODE_LOCKED,
} from "./labels";

const MODES: CodePermissionMode[] = ["plan", "ask", "auto", "allow"];

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
      <span className="inline-flex">
        {menu}
      </span>
    </WithTooltip>
  );
}

const HARNESS_ICONS: Record<
  HarnessKind,
  typeof ClaudeIcon
> = {
  claude_code: ClaudeIcon,
  codex: OpenAIIcon,
  opencode: OpenCodeIcon,
  grok: XaiIcon,
};

/** The mark for one picker row: its vendor, or the engine's own mark. */
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
  if (vendor) {
    return (
      <ProviderIcon provider={vendor} modelId={option.id} className={className} />
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
}: {
  harness: HarnessKind;
  options: readonly CodeModelOption[];
  value?: string;
  onChange?: (model: string) => void;
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
  if (!current) return null;

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
        <span className="min-w-0 flex-1 truncate text-sm">{option.label}</span>
        {index < 9 && (
          <span className="text-muted-foreground rounded-md border px-1.5 py-0.5 font-mono text-[10px]">
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
          variant="ghost"
          className="h-8 max-w-56 gap-2"
          disabled={!onChange}
          aria-label={`Model: ${current.label}`}
          title={
            onChange
              ? `Model: ${current.label}`
              : `Model: ${current.label} (set when this session started)`
          }
        >
          <CodeModelMark
            harness={harness}
            option={current}
            className="size-4 shrink-0"
          />
          <span className="truncate">{current.label}</span>
          <ChevronDown className="size-4 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        side="top"
        collisionPadding={12}
        className="model-menu-content w-80 p-0"
        onKeyDownCapture={(event) => {
          if ((event.metaKey || event.ctrlKey) && event.key >= "1" && event.key <= "9") {
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

export function CodeComposer({
  disabled,
  running,
  permissionMode,
  availableModes = MODES,
  harness,
  model,
  modelOptions,
  sessionId,
  history,
  queued = false,
  lastTurnBeganId,
  onModelChange,
  onModeChange,
  slashCommands,
  searchPaths,
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
  onModelChange?: (model: string) => void;
  onModeChange?: (mode: CodePermissionMode) => void;
  /**
   * Engine-discovered slash commands. Empty or absent hides the `/` popup;
   * free-typed `/` text still submits verbatim.
   */
  slashCommands?: readonly { name: string; description: string }[];
  /** Name-matched workspace paths for `@` completion. */
  searchPaths?: (query: string) => Promise<readonly string[]>;
  onSend: (message: string) => Promise<CodeTurnSubmission | void> | void;
  /**
   * Redirect the in-flight turn. Absent when the harness cannot steer, so a
   * mid-turn send stays on the queue path.
   */
  onSteer?: (message: string) => Promise<void>;
  onInterrupt: () => Promise<void> | void;
}) {
  const [draft, setDraft] = useState("");
  const [selectedModel, setSelectedModel] = useState(model ?? "");
  const [notice, setNotice] = useState<{ text: string } | null>(null);
  const [followUpQueued, setFollowUpQueued] = useState(false);
  const [steerPending, setSteerPending] = useState(false);
  const [steerError, setSteerError] = useState<string | null>(null);
  const [steerStatus, setSteerStatus] = useState<string | null>(null);
  const canSteer = onSteer !== undefined;
  const showQueued = queued || followUpQueued;
  const [pathItems, setPathItems] = useState<string[]>([]);
  const searchPathsRef = useRef(searchPaths);
  searchPathsRef.current = searchPaths;

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

  useEffect(() => {
    if (model) setSelectedModel(model);
  }, [model]);

  useEffect(() => {
    setFollowUpQueued(false);
  }, [lastTurnBeganId]);

  async function submit() {
    const message = draft.trim();
    if (!message || disabled) return;
    setDraft("");
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
      setDraft((current) => (current.length === 0 ? message : current));
    }
  }

  async function steer() {
    const message = draft.trim();
    if (!message || disabled || !onSteer) return;
    setSteerPending(true);
    setSteerError(null);
    setSteerStatus("Sending guidance…");
    setNotice(null);
    try {
      await onSteer(message);
      setDraft("");
      setSteerStatus("Guidance sent");
    } catch (err) {
      setSteerStatus(null);
      setSteerError(err instanceof Error ? err.message : "Could not steer");
    } finally {
      setSteerPending(false);
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
          harness && modelOptions && modelOptions.length > 0 ? (
            <HarnessModelMenu
              harness={harness}
              options={modelOptions}
              value={selectedModel || undefined}
              onChange={(next) => {
                setSelectedModel(next);
                onModelChange?.(next);
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
        pathMentions={pathMentions}
        slash={slash}
        onDraftChange={(value) => {
          setSteerError(null);
          setSteerStatus(null);
          setDraft(value);
        }}
        onSend={submit}
        onSteer={canSteer ? steer : submit}
        onQueue={submit}
        onStop={async () => {
          await onInterrupt();
        }}
        resetKey={sessionId ?? "code"}
        steerError={canSteer ? steerError : null}
        steerPending={canSteer ? steerPending : false}
        steerStatus={canSteer ? steerStatus : null}
      />
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
