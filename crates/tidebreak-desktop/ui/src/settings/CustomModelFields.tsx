import { useId, type ReactNode } from "react";

import type { ReasoningEffort } from "../api";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
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

/**
 * The fields that describe one custom model: its API id, a display name, its
 * limits, and what it accepts. Shared by the Add model form and by the review
 * step of Find models, so a row reads the same however it was started.
 */
export function CustomModelFields({
  draft,
  errors,
  acceptedEfforts,
  idLocked = false,
  disabled = false,
  onChange,
}: {
  draft: ModelDraft;
  errors: ModelDraftErrors;
  /** The reasoning levels this provider's route sends. */
  acceptedEfforts: readonly ReasoningEffort[];
  /** A discovered model keeps the id its provider reported. */
  idLocked?: boolean;
  disabled?: boolean;
  onChange: (draft: ModelDraft) => void;
}) {
  const set = (patch: Partial<ModelDraft>) => onChange({ ...draft, ...patch });
  const effortOptions = reasoningEffortOptions(acceptedEfforts);

  return (
    <div className="flex flex-col gap-4">
      <SettingsField
        label="Model ID"
        hint={fieldHint(
          errors.id,
          idLocked
            ? "As the provider reported it."
            : "Exactly as the provider's API spells it.",
        )}
      >
        <Input
          className="font-mono"
          autoComplete="off"
          spellCheck={false}
          maxLength={MAX_MODEL_ID_CHARS}
          value={draft.id}
          readOnly={idLocked}
          disabled={disabled}
          aria-invalid={errors.id ? true : undefined}
          onChange={(event) => set({ id: event.target.value })}
        />
      </SettingsField>
      <SettingsField
        label="Display name"
        hint={fieldHint(
          errors.displayName,
          "Optional. Pickers show the model ID when this is blank.",
        )}
      >
        <Input
          autoComplete="off"
          maxLength={MAX_DISPLAY_NAME_CHARS}
          value={draft.displayName}
          disabled={disabled}
          aria-invalid={errors.displayName ? true : undefined}
          onChange={(event) => set({ displayName: event.target.value })}
        />
      </SettingsField>
      <div className="grid gap-4 sm:grid-cols-2">
        <SettingsField
          label="Context window"
          hint={fieldHint(
            errors.contextWindow,
            `In tokens. Blank uses ${DEFAULT_CONTEXT_WINDOW.toLocaleString("en-US")}.`,
          )}
        >
          <Input
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
          hint={fieldHint(
            errors.maxOutputTokens,
            `In tokens. Blank uses ${DEFAULT_MAX_OUTPUT_TOKENS.toLocaleString("en-US")}.`,
          )}
        >
          <Input
            inputMode="numeric"
            autoComplete="off"
            placeholder={DEFAULT_MAX_OUTPUT_TOKENS.toLocaleString("en-US")}
            value={draft.maxOutputTokens}
            disabled={disabled}
            aria-invalid={errors.maxOutputTokens ? true : undefined}
            onChange={(event) => set({ maxOutputTokens: event.target.value })}
          />
        </SettingsField>
      </div>
      <div className="flex flex-col gap-3">
        <ToggleRow
          label="Accepts images"
          description="Images you attach go to the model."
          checked={draft.imageInput}
          disabled={disabled}
          onCheckedChange={(imageInput) => set({ imageInput })}
        />
        <ToggleRow
          label="Supports tools"
          description="Turn off to run it as a chat-only model that never calls tools."
          checked={draft.supportsTools}
          disabled={disabled}
          onCheckedChange={(supportsTools) => set({ supportsTools })}
        />
        {effortOptions.length > 0 && (
          <ToggleRow
            label="Reasons"
            description="Tidebreak asks for the provider's reasoning and lets you pick an effort level."
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
        {draft.supportsReasoning && effortOptions.length > 0 && (
          <fieldset className="flex flex-col gap-2" disabled={disabled}>
            <legend className="text-sm font-medium">
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
            <p className="text-xs text-muted-foreground">
              Leave them all clear to use the provider's default effort.
            </p>
          </fieldset>
        )}
      </div>
    </div>
  );
}

/** A field's error in critical ink, or its ordinary hint. */
function fieldHint(error: string | undefined, hint: string): ReactNode {
  return error ? <span className="text-critical">{error}</span> : hint;
}

function ToggleRow({
  label,
  description,
  checked,
  disabled,
  onCheckedChange,
}: {
  label: string;
  description: string;
  checked: boolean;
  disabled?: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  const descriptionId = useId();
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
