import type { ComponentType, FunctionComponent } from "react";
import {
  Cpu,
  Globe,
  KeyRound,
  Palette,
  PlugZap,
  RefreshCw,
  SquareTerminal,
  Waypoints,
} from "lucide-react";

import { useApp } from "@/AppContext";
import { AppearancePanel } from "./AppearancePanel";
import { CodeExecutionPanel } from "./CodeExecutionPanel";
import { GatewayPanel } from "./GatewayPanel";
import { McpPanel } from "./McpPanel";
import { ModelsPanel } from "./ModelsPanel";
import { ProvidersPanel } from "./ProvidersPanel";
import { UpdatesPanel } from "./UpdatesPanel";
import { WebSearchPanel } from "./WebSearchPanel";

/**
 * Each section reads what it needs from the shell context rather than being
 * threaded props down through a parent, so the route tree can point straight at
 * one and the rail can list them without a component in between.
 */
function ProvidersSection() {
  const { client, providers, refreshCatalog } = useApp();
  return (
    <ProvidersPanel
      providers={providers}
      client={client}
      onChanged={() => void refreshCatalog()}
    />
  );
}

function GatewaySection() {
  const { client, refreshCatalog } = useApp();
  return <GatewayPanel client={client} onChanged={() => void refreshCatalog()} />;
}

function ModelsSection() {
  const { client, models, refreshCatalog } = useApp();
  return (
    <ModelsPanel
      client={client}
      models={models}
      onChanged={() => void refreshCatalog()}
    />
  );
}

function WebSearchSection() {
  const { client } = useApp();
  return <WebSearchPanel client={client} />;
}

function CodeExecutionSection() {
  const { client } = useApp();
  return <CodeExecutionPanel client={client} />;
}

function McpSection() {
  const { client } = useApp();
  return <McpPanel client={client} />;
}

function AppearanceSection() {
  const { themeMode, setThemeMode } = useApp();
  return <AppearancePanel mode={themeMode} onChange={setThemeMode} />;
}

function UpdatesSection() {
  const { updateState, checkForUpdate, restartForUpdate } = useApp();
  return (
    <UpdatesPanel
      state={updateState}
      onCheck={checkForUpdate}
      onRestart={restartForUpdate}
    />
  );
}

export type SettingsSectionDef = {
  /** The path segment under `/settings`, and its address. */
  path: string;
  label: string;
  icon: ComponentType<{ size?: number }>;
  Component: FunctionComponent;
};

/**
 * The sections, in the order the rail lists them. The first is where a bare
 * `/settings` redirects, so it is the section a reader lands on by default.
 */
export const SETTINGS_SECTIONS: SettingsSectionDef[] = [
  { path: "providers", label: "Providers", icon: KeyRound, Component: ProvidersSection },
  { path: "gateway", label: "Model Gateway", icon: Waypoints, Component: GatewaySection },
  { path: "models", label: "Models", icon: Cpu, Component: ModelsSection },
  { path: "web-search", label: "Web search", icon: Globe, Component: WebSearchSection },
  {
    path: "code-execution",
    label: "Code execution",
    icon: SquareTerminal,
    Component: CodeExecutionSection,
  },
  { path: "mcp", label: "MCP servers", icon: PlugZap, Component: McpSection },
  { path: "appearance", label: "Appearance", icon: Palette, Component: AppearanceSection },
  { path: "updates", label: "Updates", icon: RefreshCw, Component: UpdatesSection },
];

export const DEFAULT_SETTINGS_PATH = `/settings/${SETTINGS_SECTIONS[0].path}`;
