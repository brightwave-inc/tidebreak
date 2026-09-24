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
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

/** Every mode in ascending order of autonomy. */
const ASK_PERMISSION_MODE_OPTION = {
  value: "ask",
  label: "Ask",
  icon: ShieldCheck,
} as const;

const PERMISSION_MODE_SCALE: {
  value: PermissionMode;
  label: string;
  icon: typeof ShieldCheck;
}[] = [
  {
    value: "plan",
    label: "Plan",
    icon: Eye,
  },
  ASK_PERMISSION_MODE_OPTION,
  {
    value: "auto",
    label: "Auto",
    icon: Zap,
  },
  {
    value: "allow",
    label: "Allow all",
    icon: ShieldOff,
  },
];

/** The stored mode a chat runs under when none is set. */
export const DEFAULT_PERMISSION_MODE: PermissionMode = "ask";

/**
 * What each mode does in Work, one line each, matching the permission modes
 * page of the docs. Work's own engine decides approvals, so these hold
 * there; a code engine maps the modes onto its own settings and states its
 * posture in the session header instead.
 */
export const WORK_PERMISSION_MODE_DESCRIPTIONS: Readonly<
  Record<PermissionMode, string>
> = {
  plan: "Reads and plans. Changes nothing.",
  ask: "Asks before edits and actions outside its workspace.",
  auto: "Workspace edits run. A reviewer model may approve routine actions; others still ask.",
  allow:
    "Runs every action without asking. Replacing a file in your folders still asks.",
};

/** Position on the autonomy scale, for comparing a mode against a ceiling. */
function autonomyRank(mode: PermissionMode): number {
  return PERMISSION_MODE_SCALE.findIndex((option) => option.value === mode);
}

/**
 * The mode a create or start should post under a managed ceiling. Keep the
 * requested posture when the engine supports it and policy permits it.
 * Otherwise, choose the most autonomous supported posture at or below the
 * ceiling. Return `null` when the engine has no policy-compatible posture.
 * Chat create already clamps server-side; code posts an explicit mode, so the
 * client resolves the supported posture before the request.
 */
export function clampPermissionMode(
  mode: PermissionMode,
  ceiling: PermissionMode | null | undefined,
  availableModes: readonly PermissionMode[] = PERMISSION_MODE_SCALE.map(
    (option) => option.value,
  ),
): PermissionMode | null {
  const ceilingRank =
    ceiling == null ? Number.POSITIVE_INFINITY : autonomyRank(ceiling);
  if (availableModes.includes(mode) && autonomyRank(mode) <= ceilingRank) {
    return mode;
  }

  let fallback: PermissionMode | null = null;
  for (const candidate of availableModes) {
    if (autonomyRank(candidate) > ceilingRank) continue;
    if (fallback === null || autonomyRank(candidate) > autonomyRank(fallback)) {
      fallback = candidate;
    }
  }
  return fallback;
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
 * server enforces the same ceiling at the chat routes, the turn gate, and the
 * code session routes, so this is legibility, not the lockdown itself.
 *
 * Chat's turn gate clamps an over-ceiling stored mode, so the trigger
 * displays the ceiling to match what the turn actually runs under. A code
 * session has no such clamp: pass `clampDisplay={false}` so the picker
 * shows the stored mode the engine launched with. Create surfaces clamp
 * the posted value themselves (`clampPermissionMode`) so the label and
 * the request agree without relying on this display remap.
 *
 * Work passes `descriptions`, so each row says what its mode does and the
 * trigger's tooltip names the posture in force: the choice decides whether
 * Tidebreak acts without asking, and a bare label is a guess. Code mode
 * states an engine's posture in the session header chip instead, because
 * each engine maps the modes onto its own settings.
 */
export function PermissionModeMenu({
  scopeKey,
  value,
  disabled,
  onChange,
  availableModes,
  clampDisplay = true,
  descriptions,
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
  /**
   * When true (chat), an over-ceiling stored mode displays as the ceiling
   * the turn actually runs under. When false (a live code session), the
   * trigger shows `value` so it matches the engine launch posture.
   */
  clampDisplay?: boolean;
  /** One line per mode, shown under its row and in the trigger's tooltip. */
  descriptions?: Readonly<Record<PermissionMode, string>>;
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
  const effective =
    clampDisplay && overCeiling(value ?? DEFAULT_PERMISSION_MODE)
      ? ceiling
      : value;
  const current = permissionModeOption(effective);
  const CurrentIcon = current.icon;
  const controlsDisabled = disabled || saving;
  const unavailable = (mode: PermissionMode) =>
    availableModes !== undefined && !availableModes.includes(mode);
  const guided = useGuidedMenu("permissions");
  const trigger = (
    <DropdownMenuTrigger asChild>
      <Button
        variant="ghost"
        className="h-8 min-w-0 gap-1.5"
        disabled={controlsDisabled}
        aria-label={`Permissions: ${current.label}`}
        aria-description={descriptions?.[current.value]}
        aria-busy={saving}
      >
        <CurrentIcon className="size-4 shrink-0 text-foreground" />
        <span className="truncate">{current.label}</span>
        <ChevronDown className="size-4 shrink-0 opacity-50" />
      </Button>
    </DropdownMenuTrigger>
  );
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
      {descriptions ? (
        <WithTooltip
          label={
            <>
              Permissions: {current.label}
              <span className="mt-1 block font-normal">
                {descriptions[current.value]}
              </span>
            </>
          }
          contentClassName="max-w-64"
        >
          {trigger}
        </WithTooltip>
      ) : (
        trigger
      )}
      <DropdownMenuContent
        align="end"
        side="top"
        className={descriptions ? "w-72" : "w-64"}
        data-first-task-target="permissions-menu"
        onEscapeKeyDown={guided.onEscapeKeyDown}
      >
        {PERMISSION_MODE_SCALE.map((option) => {
          const selected = current.value === option.value;
          const locked = overCeiling(option.value);
          const unoffered = unavailable(option.value);
          const OptionIcon = option.icon;
          const note = locked
            ? "Locked by your organization's policy."
            : unoffered
              ? "This engine cannot honor this mode."
              : descriptions?.[option.value];
          return (
            <DropdownMenuItem
              key={option.value}
              disabled={controlsDisabled || locked || unoffered}
              onSelect={() => {
                if (selected) return;
                void selectMode(option.value);
              }}
              className={cn(
                "py-2",
                note && "flex-col items-start gap-0.5 py-2.5",
              )}
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
              {note && (
                <span className="text-muted-foreground pl-6 text-xs">
                  {note}
                </span>
              )}
            </DropdownMenuItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
