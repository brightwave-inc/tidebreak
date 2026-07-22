import { Bot, Brain, Check, ChevronDown, Image as ImageIcon } from "lucide-react";
import type { ModelInfo } from "./api";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

/**
 * Brand marks for the providers the model registry curates. Paths are the
 * monochrome logos (from Simple Icons, CC0); they inherit `currentColor` so
 * they follow the menu's text color in both themes. Unknown providers (e.g.
 * OpenAI-compatible custom endpoints) fall back to a generic glyph.
 */
const PROVIDER_ICON_PATHS: Record<string, string> = {
  anthropic:
    "M17.3041 3.541h-3.6718l6.696 16.918H24Zm-10.6082 0L0 20.459h3.7442l1.3693-3.5527h7.0052l1.3693 3.5528h3.7442L10.5363 3.5409Zm-.3712 10.2232 2.2932-5.9456 2.2932 5.9456Z",
  openai:
    "M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.4945 4.4944zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.142.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997Z",
};

/** Brand icon for a provider, or a neutral glyph when it isn't recognized. */
export function ProviderIcon({
  provider,
  size = 14,
}: {
  provider: string;
  size?: number;
}) {
  const path = PROVIDER_ICON_PATHS[provider.toLowerCase()];
  if (!path) {
    return <Bot size={size} aria-hidden="true" />;
  }
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden="true"
    >
      <path d={path} />
    </svg>
  );
}

/** Compact token count, e.g. 200000 -> "200K", 1000000 -> "1M". */
export function formatContextWindow(tokens: number): string {
  if (tokens >= 1_000_000) {
    const millions = tokens / 1_000_000;
    return `${Number.isInteger(millions) ? millions : millions.toFixed(1)}M`;
  }
  if (tokens >= 1_000) {
    return `${Math.round(tokens / 1_000)}K`;
  }
  return `${tokens}`;
}

/**
 * Subtle capability hints for a model row: context window plus icon markers for
 * image input and adjustable reasoning effort. The reasoning marker is only an
 * indicator today — choosing an effort level needs a per-chat field the server
 * doesn't persist yet, so we surface the capability without a live control.
 */
export function ModelCapabilities({ model }: { model: ModelInfo }) {
  return (
    <div className="model-menu-item-meta">
      <span title={`${model.context_window.toLocaleString()} token context window`}>
        {formatContextWindow(model.context_window)}
      </span>
      {model.multimodal && (
        <ImageIcon size={12} aria-label="Accepts image input" />
      )}
      {model.supports_reasoning_effort && (
        <Brain size={12} aria-label="Adjustable reasoning effort" />
      )}
    </div>
  );
}

/**
 * Per-chat model selector for the message bar. `null` means "use the default"
 * (the global default model, or the server default when none is set). Mirrors
 * the Brightwave composer picker: a compact pill that opens a grouped list.
 */
export function ModelMenu({
  models,
  value,
  disabled,
  onChange,
}: {
  models: ModelInfo[];
  value: string | null;
  disabled?: boolean;
  onChange: (id: string | null) => void | Promise<void>;
}) {
  const known = models.find((m) => m.id === value);
  const label = value ? (known?.display_name ?? value) : "Default";

  // Group by provider, preserving first-seen order.
  const groups: { provider: string; models: ModelInfo[] }[] = [];
  const byProvider = new Map<string, ModelInfo[]>();
  for (const model of models) {
    const existing = byProvider.get(model.provider);
    if (existing) {
      existing.push(model);
    } else {
      const list = [model];
      byProvider.set(model.provider, list);
      groups.push({ provider: model.provider, models: list });
    }
  }

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className="model-menu-trigger"
          disabled={disabled}
          aria-label={`Model: ${label}`}
          title={`Model: ${label}`}
        >
          {known ? <ProviderIcon provider={known.provider} /> : <Bot size={14} />}
          <span className="model-menu-label">{label}</span>
          <ChevronDown size={13} />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        side="top"
        className="model-menu-content"
      >
        <DropdownMenuItem
          onSelect={() => {
            if (value !== null) void onChange(null);
          }}
        >
          <span className="model-menu-item-label">Default</span>
          {value === null && <Check className="ml-auto" />}
        </DropdownMenuItem>
        {groups.map((group) => (
          <div key={group.provider}>
            <DropdownMenuSeparator />
            <div className="model-menu-group-label">
              <ProviderIcon provider={group.provider} size={12} />
              <span>{group.provider}</span>
            </div>
            {group.models.map((model) => {
              const selected = value === model.id;
              return (
                <DropdownMenuItem
                  key={model.id}
                  onSelect={() => {
                    if (!selected) void onChange(model.id);
                  }}
                >
                  <ProviderIcon provider={model.provider} />
                  <div className="model-menu-item-main">
                    <span className="model-menu-item-label">
                      {model.display_name}
                    </span>
                    <ModelCapabilities model={model} />
                  </div>
                  {selected && <Check className="ml-auto" />}
                </DropdownMenuItem>
              );
            })}
          </div>
        ))}
        {value !== null && !known && (
          <>
            <DropdownMenuSeparator />
            <div className="model-menu-group-label">custom</div>
            <DropdownMenuItem disabled>
              <span className="model-menu-item-label">{value}</span>
              <Check className="ml-auto" />
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
