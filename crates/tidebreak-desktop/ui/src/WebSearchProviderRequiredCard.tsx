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
 *
 * Reaching this card is narrower than it used to be: Claude, GPT, and Gemini
 * chats search through their own model provider, so what is left here is a chat
 * on a model whose provider cannot search — a self-hosted or pass-through
 * route — or a host pinned to `host` mode with no key. Naming a second remedy
 * matters, because for most readers switching models is the faster one.
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
      subtitle="Add a web search provider in Settings, or switch this chat to a Claude, GPT, or Gemini model to search through it directly."
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
