import { useEffect, useState, type FormEvent } from "react";

import type { CustomModelConfig, ReasoningEffort } from "../api";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { friendlyErrorMessage } from "@/lib/utils";
import { CustomModelFields } from "./CustomModelFields";
import {
  configFromDraft,
  type ModelDraft,
  validateDraft,
} from "./customModels";
import { SettingsError } from "./primitives";

/**
 * The form for one custom model: add a new one by its API id, or edit one
 * already saved. It saves on submit and stays open with the server's message
 * when the save is refused.
 */
export function CustomModelDialog({
  open,
  onOpenChange,
  mode,
  providerName,
  requestNote,
  initial,
  acceptedEfforts,
  takenIds,
  builtInIds,
  onSave,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  mode: "add" | "edit";
  providerName: string;
  /** How this provider's requests go out, such as with the saved key. */
  requestNote: string;
  initial: ModelDraft;
  acceptedEfforts: readonly ReasoningEffort[];
  /** The ids of this provider's other custom models. */
  takenIds: ReadonlySet<string>;
  builtInIds: ReadonlySet<string>;
  /** Resolves once the row is saved; rejects with the reason it was not. */
  onSave: (model: CustomModelConfig) => Promise<void>;
}) {
  const [draft, setDraft] = useState(initial);
  const [submitted, setSubmitted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setDraft(initial);
    setSubmitted(false);
    setError(null);
  }, [open, initial]);

  const errors = validateDraft(draft, { takenIds, builtInIds });
  const hasErrors = Object.keys(errors).length > 0;

  function requestClose(next: boolean) {
    if (saving) return;
    onOpenChange(next);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (hasErrors) return;
    setSaving(true);
    setError(null);
    try {
      await onSave(configFromDraft(draft, acceptedEfforts));
      onOpenChange(false);
    } catch (err) {
      setError(friendlyErrorMessage(err, "The model could not be saved."));
    } finally {
      setSaving(false);
    }
  }

  const title =
    mode === "add"
      ? `Add a model to ${providerName}`
      : `Edit ${initial.displayName.trim() || initial.id}`;

  return (
    <Dialog open={open} onOpenChange={requestClose}>
      <DialogContent
        className="max-h-[calc(100dvh-2rem)] max-w-lg gap-5 overflow-y-auto p-5 sm:rounded-xl"
        aria-busy={saving}
        withCloseButton={!saving}
      >
        <DialogHeader className="gap-1 pr-6">
          <DialogTitle className="text-base">{title}</DialogTitle>
          <DialogDescription className="text-xs leading-relaxed">
            {requestNote} Leave a limit blank to use the default.
          </DialogDescription>
        </DialogHeader>
        <form className="flex flex-col gap-5" noValidate onSubmit={submit}>
          <CustomModelFields
            draft={draft}
            errors={submitted ? errors : {}}
            acceptedEfforts={acceptedEfforts}
            disabled={saving}
            onChange={setDraft}
          />
          {error && <SettingsError>{error}</SettingsError>}
          <DialogFooter className="gap-2 sm:justify-end">
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={saving}
              onClick={() => requestClose(false)}
            >
              Cancel
            </Button>
            <Button type="submit" size="sm" disabled={saving}>
              {saving
                ? "Saving…"
                : mode === "add"
                  ? "Add model"
                  : "Save changes"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
