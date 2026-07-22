import { useState, type ComponentType } from "react";
import { ArrowLeft, Cpu, Globe, KeyRound, Palette } from "lucide-react";
import type { ApiClient, ModelInfo, ProviderInfo } from "./api";
import type { ThemeMode } from "./theme";
import { ProvidersPanel } from "./settings/ProvidersPanel";
import { WebSearchPanel } from "./settings/WebSearchPanel";
import { ModelsPanel } from "./settings/ModelsPanel";
import { AppearancePanel } from "./settings/AppearancePanel";

type SettingsSectionKey = "providers" | "models" | "web-search" | "appearance";

const SECTIONS: {
  key: SettingsSectionKey;
  label: string;
  icon: ComponentType<{ size?: number }>;
}[] = [
  { key: "providers", label: "Providers", icon: KeyRound },
  { key: "models", label: "Models", icon: Cpu },
  { key: "web-search", label: "Web search", icon: Globe },
  { key: "appearance", label: "Appearance", icon: Palette },
];

export function SettingsView({
  client,
  models,
  providers,
  onProvidersChanged,
  onBack,
  themeMode,
  onThemeChange,
}: {
  client: ApiClient;
  models: ModelInfo[];
  providers: ProviderInfo[];
  onProvidersChanged: () => void;
  onBack: () => void;
  themeMode: ThemeMode;
  onThemeChange: (mode: ThemeMode) => void;
}) {
  const [section, setSection] = useState<SettingsSectionKey>("providers");

  return (
    <section className="settings-page">
      <nav className="settings-nav" aria-label="Settings sections">
        <button
          type="button"
          className="sidebar-action settings-back"
          onClick={onBack}
        >
          <ArrowLeft size={16} />
          Back to app
        </button>
        <div className="settings-nav-list">
          {SECTIONS.map((item) => {
            const Icon = item.icon;
            const active = section === item.key;
            return (
              <button
                key={item.key}
                type="button"
                className={`sidebar-action${active ? " is-active" : ""}`}
                aria-current={active ? "page" : undefined}
                onClick={() => setSection(item.key)}
              >
                <Icon size={16} />
                {item.label}
              </button>
            );
          })}
        </div>
      </nav>
      <div className="settings-page-content">
        {section === "providers" && (
          <ProvidersPanel
            providers={providers}
            client={client}
            onChanged={onProvidersChanged}
          />
        )}
        {section === "models" && (
          <ModelsPanel client={client} models={models} />
        )}
        {section === "web-search" && <WebSearchPanel client={client} />}
        {section === "appearance" && (
          <AppearancePanel mode={themeMode} onChange={onThemeChange} />
        )}
      </div>
    </section>
  );
}
