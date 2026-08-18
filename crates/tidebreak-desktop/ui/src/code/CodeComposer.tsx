import { useEffect, useMemo, useRef, useState } from "react";
import { Check, ChevronDown, Search } from "lucide-react";

import type { CodePermissionMode, HarnessKind, PermissionMode } from "../api/types";
import { HttpError } from "../api/client";
import { Composer } from "../Composer";
import { PermissionModeMenu } from "../PermissionModeMenu";
import {
  ClaudeIcon,
  OpenAIIcon,
  OpenCodeIcon,
  XaiIcon,
} from "../ProviderIcons";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import type { CodeTurnSubmission } from "./parsers";
import {
  type CodeModelOption,
  PERMISSION_MODE_UNAVAILABLE_REASON,
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
  return (
    <PermissionModeMenu
      scopeKey={scopeKey}
      value={value}
      disabled={!onChange}
      onChange={async (mode: PermissionMode) => {
        if (!availableModes.includes(mode)) {
          throw new Error(PERMISSION_MODE_UNAVAILABLE_REASON);
        }
        onChange?.(mode);
      }}
    />
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

export function HarnessModelMenu({
  harness,
  options,
  value,
  onChange,
}: {
  harness: HarnessKind;
  options: readonly CodeModelOption[];
  value?: string;
  onChange: (model: string) => void;
}) {
  const current =
    options.find((option) => option.id === value) ??
    options.find((option) => option.default) ??
    options[0];
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const searchInput = useRef<HTMLInputElement>(null);
  const matches = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return options;
    return options.filter(
      (option) =>
        option.label.toLowerCase().includes(needle) ||
        option.id.toLowerCase().includes(needle) ||
        option.source.toLowerCase().includes(needle),
    );
  }, [options, query]);
  if (!current) return null;
  const Icon = HARNESS_ICONS[harness];
  return (
    <DropdownMenu
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) setQuery("");
      }}
    >
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          className="h-8 max-w-56 gap-2"
          aria-label={`Model: ${current.label}`}
          title={`Model: ${current.label}`}
        >
          <Icon className="size-4 shrink-0" />
          <span className="truncate">{current.label}</span>
          <ChevronDown className="size-4 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        side="top"
        className="w-80 p-0"
        onKeyDownCapture={(event) => {
          if ((event.metaKey || event.ctrlKey) && event.key >= "1" && event.key <= "9") {
            const option = matches[Number(event.key) - 1];
            if (option) {
              event.preventDefault();
              onChange(option.id);
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
        <div className="flex max-h-72 flex-col gap-0.5 overflow-y-auto p-1">
          {matches.map((option, index) => {
            const selected = option.id === current.id;
            return (
              <DropdownMenuItem
                key={`${option.source}:${option.id}`}
                onSelect={() => onChange(option.id)}
                className="flex items-center gap-2 py-2"
              >
                <span className="flex min-w-0 flex-1 flex-col">
                  <span className="truncate">{option.label}</span>
                  <span className="text-muted-foreground truncate text-[11px]">
                    {option.source}
                  </span>
                </span>
                {index < 9 && (
                  <span className="text-muted-foreground rounded-md border px-1.5 py-0.5 font-mono text-[10px]">
                    ⌘{index + 1}
                  </span>
                )}
                {selected && <Check className="size-4 shrink-0" />}
              </DropdownMenuItem>
            );
          })}
          {matches.length === 0 && (
            <p className="text-muted-foreground px-2 py-3 text-sm">
              No models match that search.
            </p>
          )}
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
  onModelChange,
  onModeChange,
  onSend,
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
  onModelChange?: (model: string) => void;
  onModeChange?: (mode: CodePermissionMode) => void;
  onSend: (message: string) => Promise<CodeTurnSubmission | void> | void;
  onInterrupt: () => Promise<void> | void;
}) {
  const [draft, setDraft] = useState("");
  const [selectedModel, setSelectedModel] = useState(model ?? "");
  const [notice, setNotice] = useState<
    { tone: "queued" | "error"; text: string } | null
  >(null);

  useEffect(() => {
    if (model) setSelectedModel(model);
  }, [model]);

  useEffect(() => {
    if (!running) {
      setNotice((current) => (current?.tone === "queued" ? null : current));
    }
  }, [running]);

  async function submit() {
    const message = draft.trim();
    if (!message || disabled) return;
    setDraft("");
    setNotice(null);
    try {
      const outcome = await onSend(message);
      if (outcome && outcome.kind === "queued") {
        setNotice({
          tone: "queued",
          text: "Queued — runs after the current turn.",
        });
      }
    } catch (err) {
      setNotice({
        tone: "error",
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

  return (
    <div className="shrink-0 px-[clamp(0.5rem,4%,5rem)] pb-2">
      <Composer
        activeTurnId={running ? "running" : null}
        busy={running}
        cancelError={null}
        cancelPending={false}
        disabled={Boolean(disabled)}
        draft={draft}
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
        onDraftChange={setDraft}
        onSend={submit}
        onSteer={submit}
        onQueue={submit}
        onStop={async () => {
          await onInterrupt();
        }}
        resetKey={sessionId ?? "code"}
        steerError={null}
        steerPending={false}
        steerStatus={null}
      />
      {notice && (
        <p
          role="status"
          className={
            notice.tone === "error"
              ? "text-destructive mx-auto max-w-3xl pt-1 text-xs"
              : "text-muted-foreground mx-auto max-w-3xl pt-1 text-xs"
          }
        >
          {notice.text}
        </p>
      )}
    </div>
  );
}
