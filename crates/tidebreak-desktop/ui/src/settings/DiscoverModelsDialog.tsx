import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type {
  ApiClient,
  CustomModelConfig,
  DiscoveredModel,
  ProviderKind,
  ReasoningEffort,
} from "../api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { CustomModelFields } from "./CustomModelFields";
import {
  configFromDraft,
  discoveredModelFacts,
  draftFromDiscovered,
  MAX_CUSTOM_MODELS,
  type ModelDraft,
  validateDraft,
} from "./customModels";
import { SettingsError } from "./primitives";
import { Notice, NoticeRetryButton } from "@/components/ui/notice";

/** Show the filter once a listing is long enough to need one. */
const FILTER_THRESHOLD = 8;

type Listing =
  | { state: "loading" }
  | { state: "failed"; message: string }
  | { state: "ready"; models: DiscoveredModel[] };

/**
 * Find models: ask the provider which chat models the saved key can use,
 * pick some, check their limits, and add them. Nothing is saved until the
 * reader confirms the review step.
 */
export function DiscoverModelsDialog({
  open,
  onOpenChange,
  kind,
  providerName,
  usesSavedKey,
  client,
  acceptedEfforts,
  existingCount,
  onAdd,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  kind: ProviderKind;
  providerName: string;
  /** False for a local endpoint listed without a key, such as Ollama. */
  usesSavedKey: boolean;
  client: Pick<ApiClient, "discoverProviderModels">;
  acceptedEfforts: readonly ReasoningEffort[];
  /** How many custom models this provider already keeps. */
  existingCount: number;
  /** Resolves once the rows are saved; rejects with the reason they were not. */
  onAdd: (models: CustomModelConfig[]) => Promise<void>;
}) {
  const [listing, setListing] = useState<Listing>({ state: "loading" });
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [step, setStep] = useState<"pick" | "review">("pick");
  const [drafts, setDrafts] = useState<ModelDraft[]>([]);
  const [submitted, setSubmitted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Only the latest request may settle the listing: closing the dialog or
  // asking again drops whatever is still in flight.
  const request = useRef(0);
  const content = useRef<HTMLDivElement>(null);
  const load = useCallback(() => {
    const current = ++request.current;
    setListing({ state: "loading" });
    client
      .discoverProviderModels(kind)
      .then((found) => {
        if (request.current === current) {
          setListing({ state: "ready", models: found.models });
        }
      })
      .catch((err: unknown) => {
        if (request.current !== current) return;
        setListing({
          state: "failed",
          message: friendlyErrorMessage(
            err,
            `Tidebreak could not list the ${providerName} models.`,
          ),
        });
      });
  }, [client, kind, providerName]);

  useEffect(() => {
    if (!open) {
      request.current += 1;
      return;
    }
    setFilter("");
    setSelected([]);
    setStep("pick");
    setDrafts([]);
    setSubmitted(false);
    setError(null);
    load();
  }, [open, load]);

  const models = listing.state === "ready" ? listing.models : [];
  // The models the reader can still add come first; the ones Tidebreak
  // already knows follow, each group in the provider's order.
  const shown = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const matching = needle
      ? models.filter(
          (model) =>
            model.id.toLowerCase().includes(needle) ||
            (model.display_name ?? "").toLowerCase().includes(needle),
        )
      : models;
    const known = (model: DiscoveredModel) => model.built_in || model.added;
    return [
      ...matching.filter((model) => !known(model)),
      ...matching.filter(known),
    ];
  }, [filter, models]);
  const hasResults = listing.state === "ready" && models.length > 0;
  const room = Math.max(0, MAX_CUSTOM_MODELS - existingCount);

  const draftErrors = drafts.map((draft, index) =>
    validateDraft(draft, {
      takenIds: new Set(
        drafts
          .filter((_, other) => other !== index)
          .map((other) => other.id.trim()),
      ),
      builtInIds: new Set(),
    }),
  );
  const hasErrors = draftErrors.some(
    (errors) => Object.keys(errors).length > 0,
  );

  function requestClose(next: boolean) {
    if (saving) return;
    onOpenChange(next);
  }

  function toggle(id: string, checked: boolean) {
    setSelected((current) =>
      checked
        ? current.includes(id)
          ? current
          : [...current, id]
        : current.filter((entry) => entry !== id),
    );
  }

  function review() {
    const chosen = models.filter((model) => selected.includes(model.id));
    setDrafts(
      chosen.map((model) => draftFromDiscovered(model, acceptedEfforts)),
    );
    setSubmitted(false);
    setError(null);
    setStep("review");
  }

  async function add() {
    setSubmitted(true);
    if (hasErrors) return;
    setSaving(true);
    setError(null);
    try {
      await onAdd(
        drafts.map((draft) => configFromDraft(draft, acceptedEfforts)),
      );
      onOpenChange(false);
    } catch (err) {
      setError(friendlyErrorMessage(err, "The models could not be added."));
    } finally {
      setSaving(false);
    }
  }

  const count = step === "review" ? drafts.length : selected.length;
  const noun = count === 1 ? "model" : "models";

  return (
    <Dialog open={open} onOpenChange={requestClose}>
      <DialogContent
        ref={content}
        className="flex max-h-[calc(100dvh-2rem)] max-w-xl flex-col gap-4 p-5 sm:rounded-xl"
        aria-busy={listing.state === "loading" || saving}
        withCloseButton={!saving}
        // The listing arrives after the dialog opens, so its first control is
        // not there yet. Start on the dialog itself rather than on Cancel;
        // the filter takes focus when it appears.
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          content.current?.focus();
        }}
      >
        <DialogHeader className="gap-1 pr-6">
          <DialogTitle className="text-base">
            {step === "pick"
              ? `Find ${providerName} models`
              : `Check ${count} ${noun} before adding`}
          </DialogTitle>
          <DialogDescription className="text-xs leading-relaxed">
            {step === "pick"
              ? `The chat models ${providerName} lists${usesSavedKey ? " for your saved key" : ""}. Pick the ones to add; nothing is added until you confirm.`
              : "These limits come from the provider's listing. Fill in what it did not report, or leave a limit blank to use the default."}
          </DialogDescription>
        </DialogHeader>

        {step === "pick" && listing.state === "loading" && (
          <div
            className="flex items-center gap-2 py-10 text-sm text-muted-foreground"
            role="status"
          >
            <Spinner className="size-4" aria-hidden="true" />
            Asking {providerName} for its models…
          </div>
        )}

        {step === "pick" && listing.state === "failed" && (
          <Notice tone="critical" action={<NoticeRetryButton onClick={load} />}>
            {listing.message}
          </Notice>
        )}

        {step === "pick" &&
          listing.state === "ready" &&
          (models.length === 0 ? (
            <p className="py-6 text-sm text-muted-foreground">
              {providerName} listed no chat models
              {usesSavedKey ? " for this key" : ""}.
            </p>
          ) : (
            <div className="flex min-h-0 flex-1 flex-col gap-3">
              {models.length > FILTER_THRESHOLD && (
                <Input
                  type="search"
                  autoFocus
                  placeholder="Filter by name or ID"
                  aria-label="Filter models"
                  value={filter}
                  onChange={(event) => setFilter(event.target.value)}
                />
              )}
              <ul
                aria-label={`${providerName} models`}
                className="min-h-0 flex-1 overflow-y-auto rounded-md border border-border"
              >
                {shown.map((model) => (
                  <DiscoveredRow
                    key={model.id}
                    model={model}
                    checked={selected.includes(model.id)}
                    blocked={
                      !selected.includes(model.id) && selected.length >= room
                    }
                    onCheckedChange={(checked) => toggle(model.id, checked)}
                  />
                ))}
                {shown.length === 0 && (
                  <li className="px-3 py-6 text-sm text-muted-foreground">
                    No model matches &ldquo;{filter.trim()}&rdquo;.
                  </li>
                )}
              </ul>
              {room === 0 && (
                <p className="text-xs text-muted-foreground">
                  {providerName} already has {MAX_CUSTOM_MODELS} custom models,
                  the most it keeps. Remove one to add another.
                </p>
              )}
            </div>
          ))}

        {step === "review" && (
          <ol className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto pr-1">
            {drafts.map((draft, index) => (
              <li
                key={draft.id}
                className="rounded-md border border-border p-4"
                aria-label={draft.displayName.trim() || draft.id}
              >
                <CustomModelFields
                  draft={draft}
                  errors={submitted ? draftErrors[index] : {}}
                  acceptedEfforts={acceptedEfforts}
                  compact
                  disabled={saving}
                  onChange={(next) =>
                    setDrafts((current) =>
                      current.map((entry, entryIndex) =>
                        entryIndex === index ? next : entry,
                      ),
                    )
                  }
                />
              </li>
            ))}
          </ol>
        )}

        {error && <SettingsError>{error}</SettingsError>}

        <DialogFooter className="flex-row items-center justify-between gap-3 sm:justify-between">
          <span className="text-xs text-muted-foreground" aria-live="polite">
            {step === "pick" && hasResults ? `${selected.length} selected` : ""}
          </span>
          <div className="flex shrink-0 gap-2">
            {step === "pick" && !hasResults ? (
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => requestClose(false)}
              >
                Close
              </Button>
            ) : step === "pick" ? (
              <>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => requestClose(false)}
                >
                  Cancel
                </Button>
                <Button
                  type="button"
                  size="sm"
                  disabled={selected.length === 0}
                  onClick={review}
                >
                  Review {selected.length > 0 ? `${selected.length} ` : ""}
                  {selected.length === 1 ? "model" : "models"}
                </Button>
              </>
            ) : (
              <>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={saving}
                  onClick={() => setStep("pick")}
                >
                  Back
                </Button>
                <Button
                  type="button"
                  size="sm"
                  disabled={saving}
                  onClick={() => void add()}
                >
                  {saving ? "Adding…" : `Add ${count} ${noun}`}
                </Button>
              </>
            )}
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function DiscoveredRow({
  model,
  checked,
  blocked,
  onCheckedChange,
}: {
  model: DiscoveredModel;
  checked: boolean;
  /** No room for another custom model. */
  blocked: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  const known = model.built_in || model.added;
  const name = model.display_name ?? model.id;
  const facts = discoveredModelFacts(model);
  return (
    <li className="border-b border-border-subtle last:border-b-0">
      <Label
        className={cn(
          "flex items-start gap-3 px-3 py-2 font-normal leading-normal",
          !(known || blocked) && "cursor-pointer hover:bg-muted/50",
        )}
      >
        <Checkbox
          className="mt-0.5"
          checked={known || checked}
          disabled={known || blocked}
          aria-label={`Add ${name}`}
          onCheckedChange={(state) => onCheckedChange(state === true)}
        />
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate text-sm font-medium">{name}</span>
            {model.built_in && (
              <Badge variant="outline" size="sm">
                Built in
              </Badge>
            )}
            {model.added && !model.built_in && (
              <Badge variant="outline" size="sm">
                Added
              </Badge>
            )}
          </span>
          {model.display_name && (
            <span className="truncate font-mono text-xs text-muted-foreground">
              {model.id}
            </span>
          )}
          {facts.length > 0 && (
            <span className="text-xs text-muted-foreground">
              {facts.join(" · ")}
            </span>
          )}
        </span>
      </Label>
    </li>
  );
}
