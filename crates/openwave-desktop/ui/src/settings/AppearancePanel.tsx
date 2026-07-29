import { Monitor, Moon, Sun } from "lucide-react";
import type { ComponentType } from "react";
import { cn } from "@/lib/utils";
import type { ThemeMode } from "../theme";
import { SettingsField, SettingsPanel, SettingsSection } from "./primitives";

const OPTIONS: {
  mode: ThemeMode;
  label: string;
  icon: ComponentType<{ size?: number }>;
}[] = [
  { mode: "light", label: "Light", icon: Sun },
  { mode: "dark", label: "Dark", icon: Moon },
  { mode: "system", label: "System", icon: Monitor },
];

export function AppearancePanel({
  mode,
  onChange,
}: {
  mode: ThemeMode;
  onChange: (mode: ThemeMode) => void;
}) {
  return (
    <SettingsPanel
      title="Appearance"
      description="Choose how OpenWave looks. System follows your operating system setting."
    >
      <SettingsSection>
        <SettingsField label="Theme">
          <div className="theme-options" role="radiogroup" aria-label="Theme">
            {OPTIONS.map((option) => {
              const Icon = option.icon;
              const active = mode === option.mode;
              return (
                <button
                  key={option.mode}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  className={cn(
                    "inline-flex items-center gap-2 rounded-lg border border-border px-3.5 py-2 bg-background text-foreground text-[0.85rem] font-medium transition-[border-color,background-color] duration-[120ms] ease-in-out hover:bg-accent [&_svg]:text-muted-foreground",
                    active && "border-primary bg-accent [&_svg]:text-foreground",
                  )}
                  onClick={() => onChange(option.mode)}
                >
                  <Icon size={16} />
                  {option.label}
                </button>
              );
            })}
          </div>
        </SettingsField>
      </SettingsSection>
    </SettingsPanel>
  );
}
