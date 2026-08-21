import { Monitor, Moon, Sun } from "lucide-react";
import type { ComponentType } from "react";

import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
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
      description="Choose how Tidebreak looks. System follows your operating system setting."
    >
      <SettingsSection>
        <SettingsField label="Theme">
          <RadioGroup
            value={mode}
            onValueChange={(value) => onChange(value as ThemeMode)}
            className="theme-options flex flex-row flex-wrap gap-2"
            aria-label="Theme"
          >
            {OPTIONS.map((option) => {
              const Icon = option.icon;
              const active = mode === option.mode;
              return (
                <Label
                  key={option.mode}
                  className={cn(
                    "inline-flex cursor-pointer items-center gap-2 rounded-lg border border-border bg-background px-3.5 py-2 text-[0.85rem] font-medium text-foreground transition-[border-color,background-color] duration-[120ms] ease-in-out hover:bg-accent [&_svg]:text-muted-foreground",
                    active &&
                      "border-primary bg-accent [&_svg]:text-foreground",
                  )}
                >
                  <RadioGroupItem value={option.mode} className="sr-only" />
                  <Icon size={16} />
                  {option.label}
                </Label>
              );
            })}
          </RadioGroup>
        </SettingsField>
      </SettingsSection>
    </SettingsPanel>
  );
}
