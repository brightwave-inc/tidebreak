import { Settings } from "lucide-react";
import { useNavigate } from "@tanstack/react-router";

import { Button } from "@/components/ui/button";
import type { TurnFailureCategory } from "./generated/wire";
import type { ProviderKind } from "./api";
import { providerLabel } from "./ModelSelection";
import {
  Notice,
  NoticeDetail,
  NoticeRetryButton,
} from "@/components/ui/notice";

/**
 * Whether a category's recovery starts in provider settings.
 *
 * Derived from the category alone — there is no separate flag on the wire,
 * deliberately, so nothing can contradict this.
 *
 * A rejected credential or a denied account needs a fix before anything else
 * can work, so these point at settings first. Retry sits beside the settings
 * link for after the fix: the reader comes back to the same failure and
 * answers it again in place.
 */
export function turnFailurePointsAtSettings(
  category: TurnFailureCategory,
): boolean {
  return category === "auth" || category === "provider_access";
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
  provider = "the model provider",
): { title: string; body: string } {
  const titled =
    provider === "the model provider" ? "The model provider" : provider;
  switch (category) {
    case "rate_limited":
      return {
        title: `${titled} is rate-limiting requests`,
        body: "Automatic retries are already spent. Try again after demand or your provider quota resets.",
      };
    case "auth":
      // Decision 20 files a key the provider rejected and a key that was
      // never saved under this one category, and only the first reached the
      // provider. So the copy names the credential, not the provider, as
      // the problem; a provider's own words arrive as the detail below.
      return {
        title: `Tidebreak has no working credential for ${provider}`,
        body: "The API key or sign-in may be missing, expired, or rejected. Check it in provider settings, then send again.",
      };
    case "provider_access":
      return {
        title: `${titled} denied access to this request`,
        body: `This came from ${provider}, not Tidebreak. Common causes include exhausted credits or quota, billing or organization restrictions, missing model access, and key permissions.`,
      };
    case "transient":
      return {
        title: `The connection to ${provider} failed`,
        body: "The turn ended before the provider finished responding. Try again; it may succeed.",
      };
    case "unknown":
      return {
        title: "This turn could not be completed",
        body: "Tidebreak does not have a specific recovery for this failure. Try again once; if it repeats, use the detail below when troubleshooting.",
      };
  }
}

/**
 * A terminal turn failure, with whatever recovery its category offers.
 *
 * `onRetry` is supplied only for the newest failure in the transcript: a button
 * on a failure buried in scrollback would rerun a turn the reader has long
 * since moved past. A retry answers the same turn again rather than sending
 * its message a second time.
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
  const provider = model ? providerLabel(model.provider) : "the model provider";
  const copy = turnFailureCopy(category, provider);

  const pointsAtSettings = turnFailurePointsAtSettings(category);
  return (
    <Notice
      tone="critical"
      className="self-stretch"
      title={copy.title}
      action={
        (onRetry || pointsAtSettings) && (
          <>
            {pointsAtSettings && (
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => void navigate({ to: providerSettingsPath })}
              >
                <Settings aria-hidden="true" />
                Open provider settings
              </Button>
            )}
            {onRetry && <NoticeRetryButton onClick={onRetry} />}
          </>
        )
      }
    >
      <p className="text-pretty">{copy.body}</p>
      {detail && <NoticeDetail>{detail}</NoticeDetail>}
      {model && (
        <p className="mt-1.5 text-xs">
          {model.id} · {provider}
        </p>
      )}
    </Notice>
  );
}
