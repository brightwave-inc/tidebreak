import { Bot, Check, ChevronDown } from "lucide-react";
import type { ModelInfo } from "./api";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

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
          <Bot size={14} />
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
            <div className="model-menu-group-label">{group.provider}</div>
            {group.models.map((model) => {
              const selected = value === model.id;
              return (
                <DropdownMenuItem
                  key={model.id}
                  onSelect={() => {
                    if (!selected) void onChange(model.id);
                  }}
                >
                  <span className="model-menu-item-label">
                    {model.display_name}
                  </span>
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
