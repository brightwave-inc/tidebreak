import { RefreshCw, Settings } from "lucide-react";
import { useNavigate } from "@tanstack/react-router";

import { Button } from "@/components/ui/button";
import type { TurnFailureCategory } from "./generated/wire";
import type { ProviderKind } from "./api";
import { providerLabel } from "./ModelSelection";

/**
 * Whether a category's recovery is "send it again".
 *
 * Derived from the category alone — there is no separate retryable flag on the
 * wire, deliberately, so nothing can contradict this.
 *
 * `auth` is the case that must stay false: the credential the provider rejected
 * is the same one a retry would present, so the button would only replay the
 * rejection. That case points at settings instead.
 */
export function turnFailureOffersRetry(category: TurnFailureCategory): boolean {
  return category !== "auth";
}

/**
 * Renderer-owned failure copy; the server's category stays data, not prose.
 *
 * The categories only ever describe a *terminal* failure. A turn still waiting
 * on the server's own retry emits nothing at all, so `rate_limited` here means
 * those retries were already spent — copy that asks the reader to be patient
 * would be describing something that has already finished happening.
 */
export function turnFailureCopy(category: TurnFailureCategory): string {
  switch (category) {
    case "rate_limited":
      return "The model provider rate-limited this turn, and the automatic retries behind it are already spent. Sending it again may work once demand eases.";
    case "auth":
      return "The model provider rejected the credentials for this model. Sending the turn again would be rejected the same way — check the provider's API key in Settings.";
    case "transient":
      return "The connection to the model provider broke before the turn finished. Sending it again should pick up where this left off.";
    case "unknown":
      return "The turn could not be completed, and the provider gave no reason we can act on. Sending it again may not help; if it keeps failing, check the provider's API key in Settings.";
  }
}

/**
 * A terminal turn failure, with whatever recovery its category actually offers.
 *
 * `onRetry` is supplied only for the newest failure in the transcript: a button
 * on a failure buried in scrollback would resend a prompt the reader has long
 * since moved past.
 */
export function TurnFailureNotice({
  category,
  model,
  onRetry,
}: {
  category: TurnFailureCategory;
  model?: { id: string; provider: ProviderKind };
  onRetry?: () => void;
}) {
  const navigate = useNavigate();
  // Settings sections are registered from a runtime table, so TanStack's
  // generated route union contains `/settings` but not each literal child.
  const providerSettingsPath: string = "/settings/providers";

  return (
    <div className="message-notice is-error message-turn-failure" role="alert">
      <div className="message-turn-failure-text">
        {model && (
          <p className="text-muted-foreground mb-1 text-xs font-medium">
            {model.id} · {providerLabel(model.provider)}
          </p>
        )}
        <p>{turnFailureCopy(category)}</p>
      </div>
      {turnFailureOffersRetry(category) ? (
        onRetry && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="shrink-0"
            onClick={onRetry}
          >
            <RefreshCw aria-hidden="true" />
            Retry
          </Button>
        )
      ) : (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="shrink-0"
          onClick={() => void navigate({ to: providerSettingsPath })}
        >
          <Settings aria-hidden="true" />
          Open provider settings
        </Button>
      )}
    </div>
  );
}
