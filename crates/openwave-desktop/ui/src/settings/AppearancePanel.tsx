import { Monitor, Moon, Sun } from "lucide-react";
import type { ComponentType } from "react";
import type { ThemeMode } from "../theme";
import { SettingsField, SettingsPanel } from "./primitives";

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
                className={`theme-option${active ? " is-active" : ""}`}
                onClick={() => onChange(option.mode)}
              >
                <Icon size={16} />
                {option.label}
              </button>
            );
          })}
        </div>
      </SettingsField>
    </SettingsPanel>
  );
}
