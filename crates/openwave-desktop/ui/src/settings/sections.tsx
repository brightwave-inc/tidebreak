import type { ComponentType, FunctionComponent } from "react";
import { useNavigate } from "@tanstack/react-router";
import {
  Cpu,
  Globe,
  KeyRound,
  Palette,
  PlugZap,
  Quote,
  RefreshCw,
  ShieldCheck,
  SquareTerminal,
  Waypoints,
} from "lucide-react";

import { useApp } from "@/AppContext";
import { useManagedPolicy } from "@/managedPolicy";
import { AppearancePanel } from "./AppearancePanel";
import { CitationsPanel } from "./CitationsPanel";
import { CodeExecutionPanel } from "./CodeExecutionPanel";
import { GatewayPanel } from "./GatewayPanel";
import { McpPanel } from "./McpPanel";
import { PermissionsPanel } from "./PermissionsPanel";
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
  const { managed } = useManagedPolicy();
  return (
    <ProvidersPanel
      providers={providers}
      client={client}
      managed={managed}
      onChanged={() => void refreshCatalog()}
    />
  );
}

function GatewaySection() {
  const { client, refreshCatalog } = useApp();
  const navigate = useNavigate();
  // Settings sections are registered from a runtime table, so TanStack's
  // generated route union contains `/settings` but not each literal child.
  const mcpPath: string = "/settings/mcp";
  return (
    <GatewayPanel
      client={client}
      onChanged={() => void refreshCatalog()}
      onOpenMcpSettings={() => void navigate({ to: mcpPath })}
    />
  );
}

function ModelsSection() {
  const { client, models, refreshCatalog } = useApp();
  const { managed } = useManagedPolicy();
  return (
    <ModelsPanel
      client={client}
      models={models}
      managed={managed}
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
  const { managed } = useManagedPolicy();
  return <McpPanel client={client} managed={managed} />;
}

function PermissionsSection() {
  const { client } = useApp();
  return <PermissionsPanel client={client} />;
}

function CitationsSection() {
  const { client, refreshCatalog } = useApp();
  return <CitationsPanel client={client} onChanged={() => void refreshCatalog()} />;
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
  /** Kept out of the rail on a managed profile. The route still resolves — a
   * deep link or a stale history entry must land on something legible — and
   * the panel itself renders its locked state. */
  managedHidden?: boolean;
};

/**
 * The sections, in the order the rail lists them. The first is where a bare
 * `/settings` redirects, so it is the section a reader lands on by default.
 */
export const SETTINGS_SECTIONS: SettingsSectionDef[] = [
  {
    path: "providers",
    label: "Providers",
    icon: KeyRound,
    Component: ProvidersSection,
    managedHidden: true,
  },
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
  {
    path: "permissions",
    label: "Permissions",
    icon: ShieldCheck,
    Component: PermissionsSection,
  },
  { path: "citations", label: "Citations", icon: Quote, Component: CitationsSection },
  { path: "appearance", label: "Appearance", icon: Palette, Component: AppearanceSection },
  { path: "updates", label: "Updates", icon: RefreshCw, Component: UpdatesSection },
];

/**
 * The sections a profile actually navigates, in rail order.
 *
 * A managed profile has no bring-your-own credentials to manage, so the
 * Providers section is dropped and the Model Gateway — the one place its
 * models and session come from — becomes the first section, and therefore
 * where settings opens.
 */
export function settingsSectionsFor(managed: boolean): SettingsSectionDef[] {
  return managed
    ? SETTINGS_SECTIONS.filter((section) => !section.managedHidden)
    : SETTINGS_SECTIONS;
}

export function defaultSettingsPathFor(managed: boolean): string {
  return `/settings/${settingsSectionsFor(managed)[0].path}`;
}

