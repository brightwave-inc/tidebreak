import { useState, type ComponentType } from "react";
import {
  ArrowLeft,
  Cpu,
  Globe,
  KeyRound,
  Palette,
  RefreshCw,
  SquareTerminal,
} from "lucide-react";
import type { ApiClient, ModelInfo, ProviderInfo } from "./api";
import type { ThemeMode } from "./theme";
import type { DesktopUpdateState } from "./updates";
import { ProvidersPanel } from "./settings/ProvidersPanel";
import { WebSearchPanel } from "./settings/WebSearchPanel";
import { ModelsPanel } from "./settings/ModelsPanel";
import { AppearancePanel } from "./settings/AppearancePanel";
import { CodeExecutionPanel } from "./settings/CodeExecutionPanel";
import { UpdatesPanel } from "./settings/UpdatesPanel";

type SettingsSectionKey =
  | "providers"
  | "models"
  | "web-search"
  | "code-execution"
  | "appearance"
  | "updates";

const SECTIONS: {
  key: SettingsSectionKey;
  label: string;
  icon: ComponentType<{ size?: number }>;
}[] = [
  { key: "providers", label: "Providers", icon: KeyRound },
  { key: "models", label: "Models", icon: Cpu },
  { key: "web-search", label: "Web search", icon: Globe },
  { key: "code-execution", label: "Code execution", icon: SquareTerminal },
  { key: "appearance", label: "Appearance", icon: Palette },
  { key: "updates", label: "Updates", icon: RefreshCw },
];

export function SettingsView({
  client,
  models,
  providers,
  onProvidersChanged,
  onBack,
  themeMode,
  onThemeChange,
  updateState,
  onCheckForUpdate,
  onRestartForUpdate,
}: {
  client: ApiClient;
  models: ModelInfo[];
  providers: ProviderInfo[];
  onProvidersChanged: () => void;
  onBack: () => void;
  themeMode: ThemeMode;
  onThemeChange: (mode: ThemeMode) => void;
  updateState: DesktopUpdateState;
  onCheckForUpdate: () => Promise<DesktopUpdateState>;
  onRestartForUpdate: () => Promise<void>;
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
        {section === "code-execution" && (
          <CodeExecutionPanel client={client} />
        )}
        {section === "appearance" && (
          <AppearancePanel mode={themeMode} onChange={onThemeChange} />
        )}
        {section === "updates" && (
          <UpdatesPanel
            state={updateState}
            onCheck={onCheckForUpdate}
            onRestart={onRestartForUpdate}
          />
        )}
      </div>
    </section>
  );
}
