import { useState } from "react";
import { Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type {
  CodeTriggerAction,
  CodeTriggerCondition,
  CodeTriggerSnapshot,
} from "@/api/types";

/**
 * How a fire reaches the session, so the row can say what will happen rather
 * than implying every trigger interrupts.
 *
 * A harness that declares mid-turn steering is interrupted where it stands; one
 * that does not waits for the workspace to go quiet and then takes a turn.
 */
export type CodeTriggerDelivery = "steer" | "next_turn";

export type CodeTriggerTarget = {
  /** Session a fire would reach, as the interface should name it. */
  sessionTitle: string;
  harnessLabel: string;
  delivery: CodeTriggerDelivery;
};

/** Every condition, in the order the classifier settles them. */
const CONDITIONS: CodeTriggerCondition[] = [
  "checks_failed",
  "conflicts",
  "changes_requested",
  "review_required",
  "behind",
  "ready_to_merge",
  "merged",
  "closed",
  "pr_opened",
  "pr_updated",
];

function conditionCopy(condition: CodeTriggerCondition): {
  title: string;
  description: string;
} {
  switch (condition) {
    case "checks_failed":
      return {
        title: "Checks fail",
        description: "A check on the pull request reports a failure.",
      };
    case "conflicts":
      return {
        title: "Conflicts appear",
        description: "The branch stops merging cleanly into its base.",
      };
    case "changes_requested":
      return {
        title: "Changes requested",
        description: "A reviewer asks for changes.",
      };
    case "review_required":
      return {
        title: "Review outstanding",
        description: "A review or repository requirement is still unmet.",
      };
    case "behind":
      return {
        title: "Branch falls behind",
        description: "The base branch moved ahead of this one.",
      };
    case "ready_to_merge":
      return {
        title: "Ready to merge",
        description: "Nothing is outstanding. Merging stays yours.",
      };
    case "merged":
      return {
        title: "Merged",
        description: "The pull request merged.",
      };
    case "closed":
      return {
        title: "Closed",
        description: "The pull request closed without merging.",
      };
    case "pr_opened":
      return {
        title: "Pull request opens",
        description:
          "A pull request this repository's workspaces work on comes into existence.",
      };
    case "pr_updated":
      return {
        title: "Head moves",
        description:
          "A tracked pull request gets a new head. The first observed head never notifies.",
      };
  }
}

function deliveryCopy(target: CodeTriggerTarget | null): string {
  if (!target) {
    return "No session is open in a workspace with a pull request, so a fire waits until one is.";
  }
  return target.delivery === "steer"
    ? `Interrupts ${target.sessionTitle} mid-turn (${target.harnessLabel}).`
    : `Waits for ${target.sessionTitle} to go quiet, then sends a turn (${target.harnessLabel} cannot be interrupted).`;
}

export function CodeTriggerRules({
  triggers,
  target,
  busy = false,
  onArm,
  onSetEnabled,
  onChangeAction,
  onDelete,
}: {
  triggers: CodeTriggerSnapshot[];
  /** Where a fire would land right now, or null when nothing can receive one. */
  target: CodeTriggerTarget | null;
  busy?: boolean;
  /** Arm a condition that has no rule yet. */
  onArm: (condition: CodeTriggerCondition, action: CodeTriggerAction) => void;
  /**
   * Switch an existing rule on or off. Separate from arming because the rule
   * survives being switched off — turning it back on must not build a new one.
   */
  onSetEnabled: (trigger: CodeTriggerSnapshot, enabled: boolean) => void;
  onChangeAction: (
    trigger: CodeTriggerSnapshot,
    action: CodeTriggerAction,
  ) => void;
  onDelete: (trigger: CodeTriggerSnapshot) => void;
}) {
  const [draftActions, setDraftActions] = useState<
    Partial<Record<CodeTriggerCondition, CodeTriggerAction>>
  >({});
  const armed = new Map(
    triggers.map((trigger) => [trigger.condition, trigger] as const),
  );
  return (
    <div className="mx-auto max-w-3xl">
      <div className="mb-4">
        <h2 className="text-base font-semibold">Triggers</h2>
        <p className="mt-1 text-sm text-muted-foreground">
          Tidebreak watches this repository and reaches the agent when one of
          these happens, so it does not have to keep checking GitHub itself.
        </p>
        <p className="mt-1 text-xs text-muted-foreground">
          {deliveryCopy(target)}
        </p>
      </div>
      <div className="flex flex-col rounded-lg border border-border-subtle">
        {CONDITIONS.map((condition) => {
          const trigger = armed.get(condition);
          const copy = conditionCopy(condition);
          const action =
            trigger?.action ?? draftActions[condition] ?? "deliver";
          return (
            <div
              key={condition}
              className="flex flex-wrap items-center gap-4 border-b border-border-subtle px-4 py-3.5 last:border-b-0"
            >
              <Switch
                checked={Boolean(trigger?.enabled)}
                disabled={busy}
                aria-label={copy.title}
                onCheckedChange={(enabled) => {
                  // A rule that exists is switched, never rebuilt: arming
                  // again would discard the row the server keeps so its
                  // scoping survives a toggle.
                  if (trigger) {
                    onSetEnabled(trigger, enabled);
                    return;
                  }
                  if (enabled) {
                    onArm(condition, action);
                  }
                }}
              />
              <div className="min-w-52 flex-1">
                <h3 className="text-sm font-medium">{copy.title}</h3>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {copy.description}
                </p>
              </div>
              <Select
                value={action}
                disabled={busy || Boolean(trigger && !trigger.enabled)}
                onValueChange={(value) => {
                  const nextAction = value as CodeTriggerAction;
                  if (trigger) {
                    onChangeAction(trigger, nextAction);
                    return;
                  }
                  setDraftActions((current) => ({
                    ...current,
                    [condition]: nextAction,
                  }));
                }}
              >
                <SelectTrigger
                  className="w-40"
                  aria-label={`${copy.title} action`}
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="deliver">Tell the agent</SelectItem>
                  <SelectItem value="notify">Just notify me</SelectItem>
                </SelectContent>
              </Select>
              {trigger && (
                <Button
                  type="button"
                  size="icon-xs"
                  variant="ghost-destructive"
                  disabled={busy}
                  aria-label={`Delete ${copy.title} trigger`}
                  onClick={() => onDelete(trigger)}
                >
                  <Trash2 />
                </Button>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
