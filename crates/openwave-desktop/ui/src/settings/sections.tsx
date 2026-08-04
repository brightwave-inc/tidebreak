import type { ComponentType, FunctionComponent } from "react";
import { useNavigate } from "@tanstack/react-router";
import {
  Blocks,
  Bot,
  Cpu,
  Globe,
  KeyRound,
  Palette,
  RefreshCw,
  ShieldCheck,
  SquareTerminal,
  Mic,
  Waypoints,
} from "lucide-react";

import { useApp } from "@/AppContext";
import { useManagedPolicy } from "@/managedPolicy";
import { useTheme } from "@/theme";
import { AppearancePanel } from "./AppearancePanel";
import { AgentsPanel } from "./AgentsPanel";
import { CodeExecutionPanel } from "./CodeExecutionPanel";
import { ConnectedAppsPanel } from "./ConnectedAppsPanel";
import { GatewayPanel } from "./GatewayPanel";
import { PermissionsPanel } from "./PermissionsPanel";
import { ModelsPanel } from "./ModelsPanel";
import { ProvidersPanel } from "./ProvidersPanel";
import { UpdatesPanel } from "./UpdatesPanel";
import { WebSearchPanel } from "./WebSearchPanel";
import { VoiceTranscriptionPanel } from "./VoiceTranscriptionPanel";

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
  const policy = useManagedPolicy();
  const navigate = useNavigate();
  // Settings sections are registered from a runtime table, so TanStack's
  // generated route union contains `/settings` but not each literal child.
  const connectedAppsPath: string = "/settings/connected-apps";
  return (
    <GatewayPanel
      client={client}
      managed={policy.managed}
      gatewayUrl={policy.gateway_url ?? null}
      onChanged={() => void refreshCatalog()}
      onOpenConnectedApps={() => void navigate({ to: connectedAppsPath })}
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

function VoiceTranscriptionSection() {
  const { client } = useApp();
  return <VoiceTranscriptionPanel client={client} />;
}

function CodeExecutionSection() {
  const { client } = useApp();
  return <CodeExecutionPanel client={client} />;
}

function AgentsSection() {
  const { client } = useApp();
  return <AgentsPanel client={client} />;
}

function ConnectedAppsSection() {
  const { client } = useApp();
  const { managed } = useManagedPolicy();
  return <ConnectedAppsPanel client={client} managed={managed} />;
}

function PermissionsSection() {
  const { client } = useApp();
  return <PermissionsPanel client={client} />;
}

function AppearanceSection() {
  const { mode, setMode } = useTheme();
  return <AppearancePanel mode={mode} onChange={setMode} />;
}

function UpdatesSection() {
  const { updateState, updateUpToDate, checkForUpdate, restartForUpdate } =
    useApp();
  return (
    <UpdatesPanel
      state={updateState}
      upToDate={updateUpToDate}
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
  /** Kept out of the rail on an unmanaged profile, same deep-link contract:
   * the route resolves and the panel renders its not-connected state. */
  unmanagedHidden?: boolean;
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
  {
    path: "gateway",
    label: "Model Gateway",
    icon: Waypoints,
    Component: GatewaySection,
    unmanagedHidden: true,
  },
  { path: "models", label: "Models", icon: Cpu, Component: ModelsSection },
  { path: "agents", label: "Agents", icon: Bot, Component: AgentsSection },
  {
    path: "voice-transcription",
    label: "Voice input",
    icon: Mic,
    Component: VoiceTranscriptionSection,
  },
  { path: "web-search", label: "Web search", icon: Globe, Component: WebSearchSection },
  {
    path: "code-execution",
    label: "Code execution",
    icon: SquareTerminal,
    Component: CodeExecutionSection,
  },
  {
    path: "connected-apps",
    label: "Connected apps",
    icon: Blocks,
    Component: ConnectedAppsSection,
  },
  {
    path: "permissions",
    label: "Permissions",
    icon: ShieldCheck,
    Component: PermissionsSection,
  },
  { path: "appearance", label: "Appearance", icon: Palette, Component: AppearanceSection },
  { path: "updates", label: "Updates", icon: RefreshCw, Component: UpdatesSection },
];

/**
 * The sections a profile actually navigates, in rail order.
 *
 * A managed profile has no bring-your-own credentials to manage, so the
 * Providers section is dropped and the Model Gateway — the one place its
 * models and session come from — becomes the first section, and therefore
 * where settings opens. An unmanaged profile has no gateway at all — policy
 * is the only gateway source, and connecting happens from the gateway's own
 * page — so the Model Gateway section is dropped from its rail instead.
 */
export function settingsSectionsFor(managed: boolean): SettingsSectionDef[] {
  return SETTINGS_SECTIONS.filter((section) =>
    managed ? !section.managedHidden : !section.unmanagedHidden,
  );
}

export function defaultSettingsPathFor(managed: boolean): string {
  return `/settings/${settingsSectionsFor(managed)[0].path}`;
}
