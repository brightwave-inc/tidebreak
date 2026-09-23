import { useId, type ReactNode } from "react";

import type { ReasoningEffort } from "../api";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { reasoningEffortOptions } from "../ModelMenu";
import {
  DEFAULT_CONTEXT_WINDOW,
  DEFAULT_MAX_OUTPUT_TOKENS,
  MAX_DISPLAY_NAME_CHARS,
  MAX_MODEL_ID_CHARS,
  type ModelDraft,
  type ModelDraftErrors,
} from "./customModels";
import { SettingsField } from "./primitives";

/** An input the form has flagged takes the critical outline. */
const INVALID = "aria-invalid:border-critical-border";

/**
 * The fields that describe one custom model: its API id, a display name, its
 * limits, and what it accepts. Shared by the Add model form and by the review
 * step of Find models, so a row reads the same however it was started.
 *
 * `compact` is the review step's layout: the id the provider reported heads
 * the row instead of sitting in a field, the limits share one line, and the
 * hints the Add model form spells out are left to that form.
 */
export function CustomModelFields({
  draft,
  errors,
  acceptedEfforts,
  compact = false,
  disabled = false,
  onChange,
}: {
  draft: ModelDraft;
  errors: ModelDraftErrors;
  /** The reasoning levels this provider's route sends. */
  acceptedEfforts: readonly ReasoningEffort[];
  compact?: boolean;
  disabled?: boolean;
  onChange: (draft: ModelDraft) => void;
}) {
  const set = (patch: Partial<ModelDraft>) => onChange({ ...draft, ...patch });
  const effortOptions = reasoningEffortOptions(acceptedEfforts);
  const hint = (error: string | undefined, text: string): ReactNode =>
    error ? (
      <span className="text-critical">{error}</span>
    ) : compact ? undefined : (
      text
    );

  const limits = (
    <>
      <SettingsField
        label="Context window"
        hint={hint(
          errors.contextWindow,
          `In tokens. Blank uses ${DEFAULT_CONTEXT_WINDOW.toLocaleString("en-US")}.`,
        )}
      >
        <Input
          className={INVALID}
          inputMode="numeric"
          autoComplete="off"
          placeholder={DEFAULT_CONTEXT_WINDOW.toLocaleString("en-US")}
          value={draft.contextWindow}
          disabled={disabled}
          aria-invalid={errors.contextWindow ? true : undefined}
          onChange={(event) => set({ contextWindow: event.target.value })}
        />
      </SettingsField>
      <SettingsField
        label="Max output"
        hint={hint(
          errors.maxOutputTokens,
          `In tokens. Blank uses ${DEFAULT_MAX_OUTPUT_TOKENS.toLocaleString("en-US")}.`,
        )}
      >
        <Input
          className={INVALID}
          inputMode="numeric"
          autoComplete="off"
          placeholder={DEFAULT_MAX_OUTPUT_TOKENS.toLocaleString("en-US")}
          value={draft.maxOutputTokens}
          disabled={disabled}
          aria-invalid={errors.maxOutputTokens ? true : undefined}
          onChange={(event) => set({ maxOutputTokens: event.target.value })}
        />
      </SettingsField>
    </>
  );

  const displayName = (
    <SettingsField
      label="Display name"
      hint={hint(
        errors.displayName,
        "Optional. Pickers show the model ID when this is blank.",
      )}
    >
      <Input
        className={INVALID}
        autoComplete="off"
        maxLength={MAX_DISPLAY_NAME_CHARS}
        placeholder={compact ? draft.id : undefined}
        value={draft.displayName}
        disabled={disabled}
        aria-invalid={errors.displayName ? true : undefined}
        onChange={(event) => set({ displayName: event.target.value })}
      />
    </SettingsField>
  );

  return (
    <div className={cn("flex flex-col", compact ? "gap-3" : "gap-4")}>
      {compact ? (
        <div className="flex min-w-0 flex-col gap-0.5">
          <span className="truncate text-sm font-semibold">
            {draft.displayName.trim() || draft.id}
          </span>
          <span className="truncate font-mono text-xs text-muted-foreground">
            {draft.id}
          </span>
          {errors.id && (
            <span className="text-xs text-critical">{errors.id}</span>
          )}
        </div>
      ) : (
        <SettingsField
          label="Model ID"
          hint={hint(errors.id, "Exactly as the provider's API spells it.")}
        >
          <Input
            className={cn("font-mono", INVALID)}
            autoComplete="off"
            spellCheck={false}
            maxLength={MAX_MODEL_ID_CHARS}
            value={draft.id}
            disabled={disabled}
            aria-invalid={errors.id ? true : undefined}
            onChange={(event) => set({ id: event.target.value })}
          />
        </SettingsField>
      )}
      {compact ? (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
          <div className="col-span-2 sm:col-span-1">{displayName}</div>
          {limits}
        </div>
      ) : (
        <>
          {displayName}
          <div className="grid gap-4 sm:grid-cols-2">{limits}</div>
        </>
      )}
      <div
        className={cn(
          compact
            ? "flex flex-wrap items-center gap-x-6 gap-y-2"
            : "flex flex-col gap-3",
        )}
      >
        <ToggleRow
          label="Accepts images"
          description="Images you attach go to the model."
          compact={compact}
          checked={draft.imageInput}
          disabled={disabled}
          onCheckedChange={(imageInput) => set({ imageInput })}
        />
        <ToggleRow
          label="Supports tools"
          description="Turn off to run it as a chat-only model that never calls tools."
          compact={compact}
          checked={draft.supportsTools}
          disabled={disabled}
          onCheckedChange={(supportsTools) => set({ supportsTools })}
        />
        {effortOptions.length > 0 && (
          <ToggleRow
            label="Reasons"
            description="Tidebreak asks it to reason at the effort level you pick."
            compact={compact}
            checked={draft.supportsReasoning}
            disabled={disabled}
            onCheckedChange={(supportsReasoning) =>
              set({
                supportsReasoning,
                reasoningEfforts: supportsReasoning
                  ? draft.reasoningEfforts
                  : [],
              })
            }
          />
        )}
      </div>
      {draft.supportsReasoning && effortOptions.length > 0 && (
        <fieldset className="flex flex-col gap-2" disabled={disabled}>
          <legend className="mb-2 text-sm font-medium">
            Effort levels it accepts
          </legend>
          <div className="flex flex-wrap gap-x-4 gap-y-2">
            {effortOptions.map((option) => (
              <EffortCheckbox
                key={option.value}
                label={option.label}
                checked={draft.reasoningEfforts.includes(option.value)}
                onCheckedChange={(checked) =>
                  set({
                    reasoningEfforts: checked
                      ? [...draft.reasoningEfforts, option.value]
                      : draft.reasoningEfforts.filter(
                          (effort) => effort !== option.value,
                        ),
                  })
                }
              />
            ))}
          </div>
          {!compact && (
            <p className="text-xs text-muted-foreground">
              Leave them all clear to use the provider's default effort.
            </p>
          )}
        </fieldset>
      )}
    </div>
  );
}

function ToggleRow({
  label,
  description,
  compact,
  checked,
  disabled,
  onCheckedChange,
}: {
  label: string;
  description: string;
  compact: boolean;
  checked: boolean;
  disabled?: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  const descriptionId = useId();
  if (compact) {
    return (
      <Label className="flex cursor-pointer items-center gap-2 text-sm font-normal">
        <Switch
          aria-label={label}
          checked={checked}
          disabled={disabled}
          onCheckedChange={onCheckedChange}
        />
        {label}
      </Label>
    );
  }
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="flex min-w-0 flex-col gap-0.5">
        <span className="text-sm font-medium">{label}</span>
        <span id={descriptionId} className="text-xs text-muted-foreground">
          {description}
        </span>
      </div>
      <Switch
        aria-label={label}
        aria-describedby={descriptionId}
        checked={checked}
        disabled={disabled}
        onCheckedChange={onCheckedChange}
      />
    </div>
  );
}

function EffortCheckbox({
  label,
  checked,
  onCheckedChange,
}: {
  label: string;
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  return (
    <Label className="flex cursor-pointer items-center gap-2 text-sm font-normal">
      <Checkbox
        checked={checked}
        onCheckedChange={(state) => onCheckedChange(state === true)}
      />
      {label}
    </Label>
  );
}
