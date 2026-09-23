import type { ComponentType, FunctionComponent } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import {
  Bell,
  Blocks,
  Bot,
  Cpu,
  Brain,
  Gauge,
  Globe,
  GitBranch,
  HardDrive,
  KeyRound,
  Palette,
  RefreshCw,
  ScrollText,
  ShieldCheck,
  SquareTerminal,
  Mic,
  Terminal,
  Waypoints,
  Zap,
  MessagesSquare,
} from "lucide-react";

import { useApp, useAttachedRemotely } from "@/AppContext";
import { useChatListStore } from "@/ChatListStore";
import { useManagedPolicy } from "@/managedPolicy";
import { useTheme } from "@/theme";
import { downloadDesktopUpdate, useDesktopUpdatePreferences } from "@/updates";
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
import { QuickActionsPanel } from "./QuickActionsPanel";
import { ChannelsPanel } from "./ChannelsPanel";
import { PersonalInferencePreferencesPanel } from "./PersonalInferencePreferencesPanel";
import { ChannelPreferencesPanel } from "./ChannelPreferencesPanel";
import { GitSourceControlPanel } from "./GitSourceControlPanel";
import { MemoryPanel } from "./MemoryPanel";
import { InstructionsPanel } from "./InstructionsPanel";
import { DataPrivacyPanel, nativeDataPrivacyHost } from "./DataPrivacyPanel";
import { NotificationsPanel } from "./NotificationsPanel";

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
      source={policy.source}
      provisionedAt={policy.provisioned_at ?? null}
      gatewayUrl={policy.gateway_url ?? null}
      hostedGatewayUrl={policy.hosted_gateway_url ?? null}
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
  const updatePreferences = useDesktopUpdatePreferences();
  return (
    <UpdatesPanel
      state={updateState}
      upToDate={updateUpToDate}
      preferences={updatePreferences.preferences}
      preferencesSaving={updatePreferences.saving}
      preferencesError={updatePreferences.error}
      onCheck={checkForUpdate}
      onDownload={downloadDesktopUpdate}
      onRestart={restartForUpdate}
      onAutomaticDownloadsChange={(enabled) =>
        void updatePreferences.setAutomaticDownloads(enabled)
      }
    />
  );
}

function CodingHarnessesSection() {
  const { client } = useApp();
  return <CodingHarnessesPanel client={client} />;
}

function QuickActionsSection() {
  return <QuickActionsPanel />;
}

function ChannelsSection() {
  const { client } = useApp();
  const navigate = useNavigate();
  const channelsPath: string = "/settings/channels";
  const search = useRouterState({
    select: (state) => state.location.search,
  }) as Record<string, unknown>;
  if (typeof search.grant === "string" && search.inference === "personal") {
    return (
      <PersonalInferencePreferencesPanel
        client={client}
        grantId={search.grant}
        onBack={() => void navigate({ to: channelsPath, search: {} })}
      />
    );
  }
  if (typeof search.grant === "string" && typeof search.channel === "string") {
    return (
      <ChannelPreferencesPanel
        client={client}
        grantId={search.grant}
        channelId={search.channel}
      />
    );
  }
  return (
    <ChannelsPanel
      client={client}
      onOpenInferencePreferences={(grant) =>
        void navigate({
          to: channelsPath,
          search: { grant, inference: "personal" },
        })
      }
    />
  );
}

function GitSourceControlSection() {
  const { client } = useApp();
  return <GitSourceControlPanel client={client} />;
}

function MemorySection() {
  const { client } = useApp();
  const navigate = useNavigate();
  const modelsPath: string = "/settings/models";
  return (
    <MemoryPanel
      client={client}
      onOpenModels={() => void navigate({ to: modelsPath })}
      onOpenConversation={(chatId) =>
        void navigate({ to: "/c/$chatId", params: { chatId } })
      }
    />
  );
}

function InstructionsSection() {
  const { client } = useApp();
  return <InstructionsPanel client={client} />;
}

function DataPrivacySection() {
  const { client } = useApp();
  const attachedRemotely = useAttachedRemotely();
  const chats = useChatListStore((state) => state.chats);
  const navigate = useNavigate();
  return (
    <DataPrivacyPanel
      client={client}
      host={nativeDataPrivacyHost()}
      attachedRemotely={attachedRemotely}
      conversations={chats}
      onOpenSection={(path) => {
        const to: string = `/settings/${path}`;
        void navigate({ to });
      }}
    />
  );
}

export type SettingsSectionDef = {
  /** The path segment under `/settings`, and its address. */
  path: string;
  label: string;
  /**
   * Words a reader might search for that the label does not say: the names
   * of the things inside the section, and what people call them. The command
   * palette matches on these, so "dark" finds Appearance and "mcp" finds
   * Connected apps. Earlier words rank higher, so the likeliest go first.
   */
  keywords: string;
  group: SettingsSectionGroupId;
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

export const SETTINGS_SECTION_GROUPS = [
  { id: "models", label: "Models & agents" },
  { id: "capabilities", label: "Capabilities" },
  { id: "application", label: "Application" },
] as const;

export type SettingsSectionGroupId =
  (typeof SETTINGS_SECTION_GROUPS)[number]["id"];

/**
 * The sections, in the order the rail lists them. The first is where a bare
 * `/settings` redirects, so it is the section a reader lands on by default.
 */
export const SETTINGS_SECTIONS: SettingsSectionDef[] = [
  {
    path: "providers",
    label: "Providers",
    keywords:
      "API key credentials Ollama Anthropic OpenAI Gemini xAI OpenRouter local models endpoint custom model add find discover Fireworks Together",
    group: "models",
    icon: KeyRound,
    iconClass: "text-icon-amber",
    Component: ProvidersSection,
    validateSearch: providersSearch,
    managedHidden: true,
  },
  {
    path: "gateway",
    label: "Model Gateway",
    keywords: "sign in account organization managed remote machine",
    group: "models",
    icon: Waypoints,
    iconClass: "text-icon-cyan",
    Component: GatewaySection,
  },
  {
    path: "models",
    label: "Models",
    keywords: "default model prompt caching reasoning",
    group: "models",
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
    keywords: "compaction context window summarize tokens",
    group: "models",
    icon: Gauge,
    iconClass: "text-icon-blue",
    Component: CompactionSection,
  },
  {
    path: "agents",
    label: "Agents",
    keywords: "subagents delegated recaps queue steer",
    group: "models",
    icon: Bot,
    iconClass: "text-icon-violet",
    Component: AgentsSection,
  },
  {
    path: "voice-transcription",
    label: "Voice input",
    keywords: "voice microphone dictation speech transcription",
    group: "capabilities",
    icon: Mic,
    iconClass: "text-icon-rose",
    Component: VoiceTranscriptionSection,
  },
  {
    path: "web-search",
    label: "Web search",
    keywords: "internet Brave Tavily Exa Firecrawl SearXNG keys",
    group: "capabilities",
    icon: Globe,
    iconClass: "text-icon-cyan",
    Component: WebSearchSection,
  },
  {
    path: "code-execution",
    label: "Code execution",
    keywords: "sandbox Docker E2B Daytona container network keys",
    group: "capabilities",
    icon: SquareTerminal,
    iconClass: "text-icon-green",
    Component: ExecSection,
  },
  {
    path: "coding-harnesses",
    label: "Coding engines",
    keywords: "Claude Code Codex opencode Grok worktree editor",
    group: "capabilities",
    icon: Terminal,
    iconClass: "text-icon-amber",
    Component: CodingHarnessesSection,
  },
  {
    path: "quick-actions",
    label: "Quick actions",
    keywords: "prompts create PR workspace actions",
    group: "capabilities",
    icon: Zap,
    iconClass: "text-icon-violet",
    Component: QuickActionsSection,
  },
  {
    path: "channels",
    label: "Channels",
    keywords: "Slack people workspaces",
    group: "capabilities",
    icon: MessagesSquare,
    iconClass: "text-icon-green",
    Component: ChannelsSection,
    validateSearch: (search: Record<string, unknown>) => ({
      grant: typeof search.grant === "string" ? search.grant : undefined,
      channel: typeof search.channel === "string" ? search.channel : undefined,
      inference: search.inference === "personal" ? "personal" : undefined,
    }),
  },
  {
    path: "connected-apps",
    label: "Connected apps",
    keywords: "MCP servers tools integrations REST OpenAPI",
    group: "capabilities",
    icon: Blocks,
    iconClass: "text-icon-blue",
    Component: ConnectedAppsSection,
  },
  {
    path: "git-source-control",
    label: "Git & source control",
    keywords: "git branches branch prefix fetch",
    group: "application",
    icon: GitBranch,
    iconClass: "text-icon-blue",
    Component: GitSourceControlSection,
  },
  {
    path: "permissions",
    label: "Permissions",
    keywords:
      "approvals allow consent folders computer use accessibility screen recording",
    group: "application",
    icon: ShieldCheck,
    iconClass: "text-icon-green",
    Component: PermissionsSection,
  },
  {
    path: "appearance",
    label: "Appearance",
    keywords: "theme dark mode light system",
    group: "application",
    icon: Palette,
    iconClass: "text-icon-rose",
    Component: AppearanceSection,
  },
  {
    path: "notifications",
    label: "Notifications",
    keywords:
      "alerts desktop banners dock bounce approvals questions plans finished failed",
    group: "application",
    icon: Bell,
    iconClass: "text-icon-amber",
    Component: NotificationsPanel,
  },
  {
    path: "updates",
    label: "Updates",
    keywords:
      "update version release restart download automatically background up to date",
    group: "application",
    icon: RefreshCw,
    iconClass: "text-icon-green",
    Component: UpdatesSection,
  },
  {
    path: "memory",
    label: "Memory",
    keywords: "remember memories learned",
    group: "application",
    icon: Brain,
    iconClass: "text-icon-violet",
    Component: MemorySection,
  },
  // Beside Memory: both shape how Tidebreak answers you, one learned as you
  // go and one written up front.
  {
    path: "instructions",
    label: "Instructions",
    keywords:
      "custom instructions personal project preferences system prompt style tone",
    group: "application",
    icon: ScrollText,
    iconClass: "text-icon-amber",
    Component: InstructionsSection,
  },
  {
    path: "data-privacy",
    label: "Data and privacy",
    keywords:
      "backup back up export delete erase reset privacy storage disk space folder location telemetry network",
    group: "application",
    icon: HardDrive,
    iconClass: "text-icon-cyan",
    Component: DataPrivacySection,
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

export function settingsSectionGroupsFor(managed: boolean) {
  const sections = settingsSectionsFor(managed);
  return SETTINGS_SECTION_GROUPS.map((group) => ({
    ...group,
    sections: sections.filter((section) => section.group === group.id),
  })).filter((group) => group.sections.length > 0);
}

export function defaultSettingsPathFor(managed: boolean): string {
  return `/settings/${settingsSectionsFor(managed)[0].path}`;
}
