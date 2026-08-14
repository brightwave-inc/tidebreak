import { AlertCircle, RefreshCw, Settings } from "lucide-react";
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
  return category !== "auth" && category !== "provider_access";
}

/**
 * Renderer-owned failure copy; the server's category stays data, not prose.
 *
 * The categories only ever describe a *terminal* failure. A turn still waiting
 * on the server's own retry emits nothing at all, so `rate_limited` here means
 * those retries were already spent — copy that asks the reader to be patient
 * would be describing something that has already finished happening.
 */
export function turnFailureCopy(
  category: TurnFailureCategory,
  provider = "The model provider",
): { title: string; body: string } {
  switch (category) {
    case "rate_limited":
      return {
        title: `${provider} is rate-limiting requests`,
        body: "Automatic retries are already spent. Retry after demand or your provider quota resets.",
      };
    case "auth":
      return {
        title: `${provider} could not authenticate this request`,
        body: "Check that the API key is present, active, and belongs to the account or organization you intended to use.",
      };
    case "provider_access":
      return {
        title: `${provider} denied access to this request`,
        body: `This came from ${provider}, not Tidebreak. Common causes include exhausted credits or quota, billing or organization restrictions, missing model access, and key permissions.`,
      };
    case "transient":
      return {
        title: `The connection to ${provider} failed`,
        body: "The turn ended before the provider finished responding. Retrying may succeed.",
      };
    case "unknown":
      return {
        title: "This turn could not be completed",
        body: "Tidebreak does not have a specific recovery for this failure. Retry once; if it repeats, use the detail below when troubleshooting.",
      };
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
  detail,
  model,
  onRetry,
}: {
  category: TurnFailureCategory;
  detail?: string;
  model?: { id: string; provider: ProviderKind };
  onRetry?: () => void;
}) {
  const navigate = useNavigate();
  // Settings sections are registered from a runtime table, so TanStack's
  // generated route union contains `/settings` but not each literal child.
  const providerSettingsPath: string = "/settings/providers";
  const provider = model ? providerLabel(model.provider) : "The model provider";
  const copy = turnFailureCopy(category, provider);

  return (
    <aside className="message-turn-failure" role="alert">
      <AlertCircle className="message-turn-failure-icon" aria-hidden="true" />
      <div className="message-turn-failure-text">
        <p className="message-turn-failure-title">{copy.title}</p>
        <p className="message-turn-failure-body">{copy.body}</p>
        {detail && <code className="message-turn-failure-detail">{detail}</code>}
        {model && (
          <p className="message-turn-failure-model">
            {model.id} · {provider}
          </p>
        )}
      </div>
      {turnFailureOffersRetry(category) ? (
        onRetry && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="message-turn-failure-action"
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
          className="message-turn-failure-action"
          onClick={() => void navigate({ to: providerSettingsPath })}
        >
          <Settings aria-hidden="true" />
          Open provider settings
        </Button>
      )}
    </aside>
  );
}
