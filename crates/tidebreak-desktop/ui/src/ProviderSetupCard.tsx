import { useId, type ReactNode } from "react";
import { ArrowUpRight, KeyRound } from "lucide-react";

import type { ProviderKind } from "./api";
import { providerLabel } from "./ModelSelection";
import { ProviderIcon } from "./ProviderIcons";
import { Button } from "@/components/ui/button";
import { hostMachineLabel } from "@/remoteMachine";

/** Where a setup choice leads: one provider's card, or the whole list. */
export type ProviderSetupTarget = {
  provider: ProviderKind | null;
  /** Put the cursor in the card's key field. */
  focusCredential: boolean;
};

/** The providers the key row names first, in picker order. */
const KEY_PROVIDERS: readonly ProviderKind[] = [
  "anthropic",
  "openai",
  "gemini",
  "xai",
];

const ROW_CLASS =
  "provider-setup-row group flex w-full items-start gap-3 px-4 py-3 short:py-2 text-left transition-colors hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none";

function SetupRow({
  icon,
  title,
  description,
  onClick,
}: {
  icon: ReactNode;
  title: string;
  description: string;
  onClick: () => void;
}) {
  return (
    <button type="button" className={ROW_CLASS} onClick={onClick}>
      <span className="provider-setup-icon mt-0.5 flex size-4 shrink-0 items-center justify-center text-muted-foreground">
        {icon}
      </span>
      <span className="provider-setup-copy min-w-0 flex-1">
        <span className="provider-setup-title block text-sm font-medium text-foreground">
          {title}
        </span>
        <span className="provider-setup-desc block text-xs leading-5 text-muted-foreground short:truncate">
          {description}
        </span>
      </span>
      <ArrowUpRight
        aria-hidden="true"
        className="provider-setup-go mt-0.5 size-4 shrink-0 text-muted-foreground transition-colors group-hover:text-foreground"
      />
    </button>
  );
}

/**
 * What home offers when no model can run: the ways to connect one, each
 * opening its card in Settings → Providers. It stands where the starter
 * prompts go, because a starter sent with no model can only fail.
 */
export function ProviderSetupCard({
  onSetUp,
}: {
  onSetUp: (target: ProviderSetupTarget) => void;
}) {
  const keysLabelId = useId();
  return (
    <nav
      className="provider-setup w-full overflow-hidden rounded-xl border border-border bg-background text-left"
      aria-label="Connect a model"
    >
      <ul className="divide-y divide-border-subtle">
        <li>
          <SetupRow
            icon={<ProviderIcon provider="openai" className="size-4" />}
            title="Sign in with ChatGPT"
            description="Use your ChatGPT Plus or Pro plan."
            onClick={() =>
              onSetUp({ provider: "openai", focusCredential: false })
            }
          />
        </li>
        <li className="provider-setup-row flex items-start gap-3 px-4 py-3 short:py-2">
          <span className="provider-setup-icon mt-0.5 flex size-4 shrink-0 items-center justify-center text-muted-foreground">
            <KeyRound className="size-4" aria-hidden="true" />
          </span>
          <span className="provider-setup-copy min-w-0 flex-1">
            <span
              className="provider-setup-title block text-sm font-medium text-foreground"
              id={keysLabelId}
            >
              Add an API key
            </span>
            <span
              className="provider-setup-keys mt-1.5 flex flex-wrap gap-1.5 short:mt-1"
              role="group"
              aria-labelledby={keysLabelId}
            >
              {KEY_PROVIDERS.map((provider) => (
                <Button
                  key={provider}
                  type="button"
                  variant="outline"
                  size="xs"
                  onClick={() => onSetUp({ provider, focusCredential: true })}
                >
                  <ProviderIcon provider={provider} />
                  {providerLabel(provider)}
                </Button>
              ))}
              <Button
                type="button"
                variant="outline"
                size="xs"
                className="text-muted-foreground"
                onClick={() =>
                  onSetUp({ provider: null, focusCredential: false })
                }
              >
                More providers
              </Button>
            </span>
          </span>
        </li>
        <li>
          <SetupRow
            icon={<ProviderIcon provider="ollama" className="size-4" />}
            title="Connect Ollama"
            description={`Run open models on ${hostMachineLabel()}. No key needed.`}
            onClick={() =>
              onSetUp({ provider: "ollama", focusCredential: false })
            }
          />
        </li>
        <li>
          <SetupRow
            icon={
              <ProviderIcon provider="openai_compatible" className="size-4" />
            }
            title="Connect an OpenAI-compatible server"
            description="LM Studio, llama.cpp, vLLM, or your own endpoint."
            onClick={() =>
              onSetUp({ provider: "openai_compatible", focusCredential: false })
            }
          />
        </li>
      </ul>
    </nav>
  );
}

/**
 * What home says on a managed profile with nothing to run: the models come
 * from the organization's gateway, so there is no key to add here.
 */
export function ManagedModelNotice({
  onOpenGateway,
}: {
  onOpenGateway: () => void;
}) {
  return (
    <div className="provider-setup w-full overflow-hidden rounded-xl border border-border bg-background text-left">
      <SetupRow
        icon={<ProviderIcon provider="model_gateway" className="size-4" />}
        title="Open Model Gateway settings"
        description="Sign in to your gateway, or ask your administrator for access."
        onClick={onOpenGateway}
      />
    </div>
  );
}
