import {
  Check,
  ChevronDown,
  Eye,
  Lock,
  ShieldCheck,
  ShieldOff,
  Zap,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import type { PermissionMode } from "./api";
import { useManagedPolicy } from "./managedPolicy";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { useGuidedMenu } from "./FirstTaskWalkthrough";
import { cn } from "@/lib/utils";

/**
 * Every mode in ascending order of autonomy, with its menu copy.
 *
 * The descriptions state the decision order the server actually applies:
 * saved allowlists run without asking in every mode, and the mode governs
 * only the calls no grant covers.
 */
const ASK_PERMISSION_MODE_OPTION = {
  value: "ask",
  label: "Ask",
  description:
    "Ask before edits and actions that leave the workspace. Allowlists you've saved still run without asking.",
  icon: ShieldCheck,
} as const;

const PERMISSION_MODE_SCALE: {
  value: PermissionMode;
  label: string;
  description: string;
  icon: typeof ShieldCheck;
}[] = [
  {
    value: "plan",
    label: "Plan",
    description:
      "Read-only: the agent explores and proposes a plan. Nothing is edited or run until you switch modes.",
    icon: Eye,
  },
  ASK_PERMISSION_MODE_OPTION,
  {
    value: "auto",
    label: "Auto",
    description:
      "Workspace edits run on their own; anything that leaves the workspace still asks.",
    icon: Zap,
  },
  {
    value: "allow",
    label: "Allow all",
    description: "Everything runs without asking, in this work only.",
    icon: ShieldOff,
  },
];

/** The stored mode a chat runs under when none is set. */
export const DEFAULT_PERMISSION_MODE: PermissionMode = "ask";

/** Position on the autonomy scale, for comparing a mode against a ceiling. */
function autonomyRank(mode: PermissionMode): number {
  return PERMISSION_MODE_SCALE.findIndex((option) => option.value === mode);
}

export function permissionModeOption(mode: PermissionMode | null) {
  const value = mode ?? DEFAULT_PERMISSION_MODE;
  return (
    PERMISSION_MODE_SCALE.find((option) => option.value === value) ??
    ASK_PERMISSION_MODE_OPTION
  );
}

/**
 * Per-chat permission-mode selector, shown beside the model picker. `null`
 * reads as Ask; selecting Ask stores the explicit token rather than clearing,
 * so a chat that was deliberately dialed back stays that way if the default
 * ever changes.
 *
 * The mode icons stay in the same neutral palette as the rest of the app
 * chrome. Selection, rather than autonomy level, supplies the visual emphasis;
 * higher-autonomy choices are normal operating modes, not warning states.
 *
 * A managed profile may assert a permission-mode ceiling. Modes above it
 * render locked — decided elsewhere rather than silently missing — and the
 * server enforces the same ceiling at the chat routes and the turn gate, so
 * this is legibility, not the lockdown itself. A stored mode already above
 * the ceiling displays as the ceiling, matching what the turn actually runs
 * under.
 */
export function PermissionModeMenu({
  scopeKey,
  value,
  disabled,
  onChange,
  availableModes,
  open: controlledOpen,
  onOpenChange,
}: {
  /** Identity of the chat or draft whose setting is being changed. */
  scopeKey: string;
  value: PermissionMode | null;
  disabled?: boolean;
  onChange: (mode: PermissionMode) => void | Promise<void>;
  /**
   * Modes the engine behind this surface honors. Absent means all of them —
   * chat's server runs every mode. Rows outside the list stay visible and
   * disabled, so what an engine cannot do is stated rather than missing.
   */
  availableModes?: readonly PermissionMode[];
  /** Open the menu from outside — a surface's keyboard shortcut. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  const [saving, setSaving] = useState(false);
  const savingRef = useRef(false);
  const operationGenerationRef = useRef(0);
  const scopeKeyRef = useRef(scopeKey);
  scopeKeyRef.current = scopeKey;

  useEffect(() => {
    operationGenerationRef.current += 1;
    savingRef.current = false;
    setSaving(false);
    return () => {
      operationGenerationRef.current += 1;
      savingRef.current = false;
    };
  }, [scopeKey]);

  async function selectMode(mode: PermissionMode) {
    if (savingRef.current) return;
    const startingScope = scopeKey;
    const generation = ++operationGenerationRef.current;
    savingRef.current = true;
    setSaving(true);
    try {
      await onChange(mode);
    } catch {
      if (
        scopeKeyRef.current === startingScope &&
        operationGenerationRef.current === generation
      ) {
        toast.error("Could not update permissions. Try again.");
      }
    } finally {
      if (
        scopeKeyRef.current === startingScope &&
        operationGenerationRef.current === generation
      ) {
        savingRef.current = false;
        setSaving(false);
      }
    }
  }

  const ceiling = useManagedPolicy().permission_mode_ceiling ?? null;
  const ceilingRank = ceiling === null ? null : autonomyRank(ceiling);
  const overCeiling = (mode: PermissionMode) =>
    ceilingRank !== null && autonomyRank(mode) > ceilingRank;
  const effective = overCeiling(value ?? DEFAULT_PERMISSION_MODE)
    ? ceiling
    : value;
  const current = permissionModeOption(effective);
  const CurrentIcon = current.icon;
  const controlsDisabled = disabled || saving;
  const unavailable = (mode: PermissionMode) =>
    availableModes !== undefined && !availableModes.includes(mode);
  const guided = useGuidedMenu("permissions");
  return (
    <DropdownMenu
      open={
        controlledOpen !== undefined
          ? controlledOpen || guided.open
          : guided.open
      }
      modal={guided.modal}
      onOpenChange={(next) => {
        guided.onOpenChange(next);
        onOpenChange?.(next);
      }}
    >
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          className="h-8 min-w-0 gap-1.5"
          disabled={controlsDisabled}
          aria-label={`Permissions: ${current.label}`}
          aria-busy={saving}
        >
          <CurrentIcon className="size-4 shrink-0 text-foreground" />
          <span className="truncate">{current.label}</span>
          <ChevronDown className="size-4 shrink-0 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="end"
        side="top"
        className="w-72"
        data-first-task-target="permissions-menu"
        onEscapeKeyDown={guided.onEscapeKeyDown}
      >
        {PERMISSION_MODE_SCALE.map((option) => {
          const selected = current.value === option.value;
          const locked = overCeiling(option.value);
          const unoffered = unavailable(option.value);
          const OptionIcon = option.icon;
          return (
            <DropdownMenuItem
              key={option.value}
              disabled={controlsDisabled || locked || unoffered}
              onSelect={() => {
                if (selected) return;
                void selectMode(option.value);
              }}
              className="flex flex-col items-start gap-0.5 py-3"
              data-first-task-target={
                option.value === "ask" ? "permissions-ask" : undefined
              }
            >
              <div className="flex w-full items-center justify-between">
                <span className="flex items-center gap-2 font-medium">
                  <OptionIcon
                    className={cn(
                      "size-4",
                      selected && !locked
                        ? "text-foreground"
                        : "text-muted-foreground",
                    )}
                  />
                  {option.label}
                </span>
                {locked ? (
                  <Lock aria-label="Locked" className="size-4" />
                ) : (
                  selected && <Check className="size-4" />
                )}
              </div>
              <span className="text-muted-foreground pl-6 text-xs">
                {locked
                  ? "Locked by your organization's policy."
                  : unoffered
                    ? "This harness can't honor this mode."
                    : option.description}
              </span>
            </DropdownMenuItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
