import { useId } from "react";
import { useNavigate } from "@tanstack/react-router";
import { Globe } from "lucide-react";

import { Button } from "@/components/ui/button";

/** An actionable, renderer-owned explanation of an unconfigured web search. */
export function WebSearchProviderRequiredCard() {
  const titleId = useId();
  const navigate = useNavigate();
  // Settings sections are registered from a runtime table, so TanStack's
  // generated route union contains `/settings` but not each literal child.
  const settingsPath: string = "/settings/web-search";

  return (
    <section
      className="bg-background flex max-w-prose items-start gap-3 rounded-lg border p-4"
      aria-labelledby={titleId}
    >
      <Globe
        className="text-muted-foreground mt-0.5 size-5 shrink-0"
        aria-hidden="true"
      />
      <div className="flex min-w-0 flex-1 flex-col items-start gap-3">
        <div className="space-y-1">
          <h2 id={titleId} className="text-sm font-medium">
            Web search needs a provider
          </h2>
          <p className="text-muted-foreground text-sm">
            Choose a web search provider and add its API key in Settings.
          </p>
        </div>
        <Button
          type="button"
          variant="primary"
          size="sm"
          onClick={() => void navigate({ to: settingsPath })}
        >
          Configure web search
        </Button>
      </div>
    </section>
  );
}
