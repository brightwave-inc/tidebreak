import type { ComponentType, FunctionComponent } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import {
  Blocks,
  Bot,
  Cpu,
  Gauge,
  Globe,
  KeyRound,
  Palette,
  RefreshCw,
  ShieldCheck,
  SquareTerminal,
  Mic,
  Terminal,
  Waypoints,
} from "lucide-react";

import { useApp } from "@/AppContext";
import { useChatListStore } from "@/ChatListStore";
import { useManagedPolicy } from "@/managedPolicy";
import { useTheme } from "@/theme";
import { AppearancePanel } from "./AppearancePanel";
import { AgentsPanel } from "./AgentsPanel";
import { ExecPanel } from "./ExecPanel";
import { CompactionPanel } from "./CompactionPanel";
import { ConnectedAppsPanel } from "./ConnectedAppsPanel";
import { GatewayPanel } from "./GatewayPanel";
import { PermissionsPanel } from "./PermissionsPanel";
import { ModelsPanel } from "./ModelsPanel";
import { ProvidersPanel } from "./ProvidersPanel";
import { UpdatesPanel } from "./UpdatesPanel";
import { WebSearchPanel } from "./WebSearchPanel";
import { VoiceTranscriptionPanel } from "./VoiceTranscriptionPanel";
import { CodingHarnessesPanel } from "./CodingHarnessesPanel";

/**
 * Each section reads what it needs from the shell context rather than being
 * threaded props down through a parent, so the route tree can point straight at
 * one and the rail can list them without a component in between.
 */
/**
 * Where a link into Providers is pointing: the card to open, and whether the
 * cursor belongs in its credential field. Anything else in the URL is dropped
 * — a stale or hand-edited link still lands on a legible page.
 */
export type ProvidersSearch = {
  provider?: string;
  focus?: "credential";
};

export function providersSearch(
  search: Record<string, unknown>,
): ProvidersSearch {
  return {
    provider: typeof search.provider === "string" ? search.provider : undefined,
    focus: search.focus === "credential" ? "credential" : undefined,
  };
}

function ProvidersSection() {
  const { client, models, providers, refreshCatalog } = useApp();
  const { managed } = useManagedPolicy();
  const search = providersSearch(
    useRouterState({ select: (state) => state.location.search }) as Record<
      string,
      unknown
    >,
  );
  return (
    <ProvidersPanel
      providers={providers}
      models={models}
      client={client}
      managed={managed}
      onChanged={() => void refreshCatalog()}
      expandProvider={search.provider}
      focusCredential={search.focus === "credential"}
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

function ExecSection() {
  const { client } = useApp();
  return <ExecPanel client={client} />;
}

function CompactionSection() {
  const { client } = useApp();
  return <CompactionPanel client={client} />;
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
  const chats = useChatListStore((state) => state.chats);
  const knownChatIds = new Set(chats.map((chat) => chat.id));
  const knownProjectIds = new Set(
    chats
      .map((chat) => chat.project_id)
      .filter((id): id is string => id != null),
  );
  return (
    <PermissionsPanel
      client={client}
      knownChatIds={knownChatIds}
      knownProjectIds={knownProjectIds}
    />
  );
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

function CodingHarnessesSection() {
  const { client } = useApp();
  return <CodingHarnessesPanel client={client} />;
}

export type SettingsSectionDef = {
  /** The path segment under `/settings`, and its address. */
  path: string;
  label: string;
  icon: ComponentType<{ size?: number; className?: string }>;
  iconClass: string;
  Component: FunctionComponent;
  /** Search params this section addresses with, validated at the route so an
   * unknown value never reaches the panel. */
  validateSearch?: (search: Record<string, unknown>) => Record<string, unknown>;
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
    iconClass: "text-icon-amber",
    Component: ProvidersSection,
    validateSearch: providersSearch,
    managedHidden: true,
  },
  {
    path: "gateway",
    label: "Model Gateway",
    icon: Waypoints,
    iconClass: "text-icon-cyan",
    Component: GatewaySection,
  },
  {
    path: "models",
    label: "Models",
    icon: Cpu,
    iconClass: "text-icon-violet",
    Component: ModelsSection,
  },
  // Next to Models rather than in a section of its own: every number here is a
  // fraction of the selected model's context window, so a reader who has just
  // changed models is in the right place to reconsider them.
  {
    path: "context",
    label: "Context",
    icon: Gauge,
    iconClass: "text-icon-blue",
    Component: CompactionSection,
  },
  {
    path: "agents",
    label: "Agents",
    icon: Bot,
    iconClass: "text-icon-violet",
    Component: AgentsSection,
  },
  {
    path: "voice-transcription",
    label: "Voice input",
    icon: Mic,
    iconClass: "text-icon-rose",
    Component: VoiceTranscriptionSection,
  },
  {
    path: "web-search",
    label: "Web search",
    icon: Globe,
    iconClass: "text-icon-cyan",
    Component: WebSearchSection,
  },
  {
    path: "code-execution",
    label: "Code execution",
    icon: SquareTerminal,
    iconClass: "text-icon-green",
    Component: ExecSection,
  },
  {
    path: "coding-harnesses",
    label: "Coding harnesses",
    icon: Terminal,
    iconClass: "text-icon-amber",
    Component: CodingHarnessesSection,
  },
  {
    path: "connected-apps",
    label: "Connected apps",
    icon: Blocks,
    iconClass: "text-icon-blue",
    Component: ConnectedAppsSection,
  },
  {
    path: "permissions",
    label: "Permissions",
    icon: ShieldCheck,
    iconClass: "text-icon-green",
    Component: PermissionsSection,
  },
  {
    path: "appearance",
    label: "Appearance",
    icon: Palette,
    iconClass: "text-icon-rose",
    Component: AppearanceSection,
  },
  {
    path: "updates",
    label: "Updates",
    icon: RefreshCw,
    iconClass: "text-icon-green",
    Component: UpdatesSection,
  },
];

/**
 * The sections a profile actually navigates, in rail order.
 *
 * A managed profile has no bring-your-own credentials to manage, so the
 * Providers section is dropped and the Model Gateway — the one place its
 * models and session come from — becomes the first section, and therefore
 * where settings opens. Every other section is on both rails, Model Gateway
 * included: an unmanaged profile has no gateway to configure, but the
 * section is also where a machine is attached, and a machine behind no
 * gateway is reachable with its own token.
 */
export function settingsSectionsFor(managed: boolean): SettingsSectionDef[] {
  return SETTINGS_SECTIONS.filter(
    (section) => !(managed && section.managedHidden),
  );
}

export function defaultSettingsPathFor(managed: boolean): string {
  return `/settings/${settingsSectionsFor(managed)[0].path}`;
}
