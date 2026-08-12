import { useId } from "react";
import { useNavigate } from "@tanstack/react-router";

import { AttentionCard } from "./AttentionCard";
import { Button } from "@/components/ui/button";

/**
 * An actionable, renderer-owned explanation of an unconfigured web search.
 *
 * It wears the shared attention-card chrome, so a result that needs a decision
 * reads the same as a consent prompt or a clarifying question. The tool group
 * above already names and labels the search, so the card carries no icon of its
 * own.
 */
export function WebSearchProviderRequiredCard() {
  const titleId = useId();
  const navigate = useNavigate();
  // Settings sections are registered from a runtime table, so TanStack's
  // generated route union contains `/settings` but not each literal child.
  const settingsPath: string = "/settings/web-search";

  return (
    <AttentionCard
      title="Web search needs a provider"
      titleId={titleId}
      subtitle="Choose a web search provider and add its API key in Settings."
    >
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          size="sm"
          onClick={() => void navigate({ to: settingsPath })}
        >
          Configure web search
        </Button>
      </div>
    </AttentionCard>
  );
}
