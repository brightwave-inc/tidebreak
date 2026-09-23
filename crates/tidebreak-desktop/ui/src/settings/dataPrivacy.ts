import type { DataCategory } from "@/api/types";
import { FINISHED_KEY, NEEDS_YOU_KEY } from "@/NotificationPreferences";
import { ZOOM_KEY } from "@/InterfaceZoom";
import { STORAGE_KEY as THEME_KEY } from "@/theme";
import { SIDEBAR_COLLAPSED_KEY, SIDEBAR_WIDTH_KEY } from "@/UiStore";

/** How each part of the data folder reads on the page. */
export const DATA_CATEGORY_COPY: Record<
  DataCategory,
  { label: string; description: string }
> = {
  database: {
    label: "Database",
    description: "Conversations, memory, and settings",
  },
  attachments: {
    label: "Attachments",
    description: "Files you added to conversations",
  },
  outputs: { label: "Outputs", description: "Files Tidebreak made" },
  logs: { label: "Logs", description: "What Tidebreak did, for support" },
  engine_tools: {
    label: "Engine tools",
    description: "Node, LibreOffice, voice, and coding engines",
  },
  backups: {
    label: "Backups",
    description: "Copies saved before updates",
  },
  other: {
    label: "Other",
    description: "Working files and coding session files",
  },
};

/** One kind of traffic that leaves this computer. */
export type OutboundTraffic = {
  id: string;
  title: string;
  /** What goes where, and when. */
  what: string;
  /** How to stop it, or why nothing needs stopping. */
  off: string;
  /** The settings section that controls it. */
  section?: { path: string; label: string };
};

/**
 * Everything the app sends off this computer, and how to stop each one.
 *
 * Kept in step with the code by hand: a feature that starts calling out must
 * add a row here, in the same change.
 */
export const OUTBOUND_TRAFFIC: readonly OutboundTraffic[] = [
  {
    id: "conversations",
    title: "Conversations",
    what: "Each turn sends the conversation, its attachments, and your memory to the provider of the model you chose.",
    off: "Pick a local model, such as one from Ollama, or turn a provider off.",
    section: { path: "providers", label: "Providers" },
  },
  {
    id: "background",
    title: "Background work",
    what: "Titles, workspace names, turn recaps, memory, and approval checks in Auto mode send parts of a conversation to the background model.",
    off: "Choose a local background model in Models. Turn off recaps in Agents and memory in Memory.",
    section: { path: "models", label: "Models" },
  },
  {
    id: "web",
    title: "Web search and pages",
    what: "Searches go to your search provider, or to your model provider's own search. Pages the agent reads are fetched from this computer.",
    off: "Set Search mode to Off to stop searches. The agent still reads pages; set a conversation to Offline to stop both.",
    section: { path: "web-search", label: "Web search" },
  },
  {
    id: "updates",
    title: "Update checks",
    what: "Tidebreak checks downloads.brightwave.io for a new version at launch and every hour, and downloads it unless you turn that off.",
    off: "Turn off Download updates automatically. The check itself always runs.",
    section: { path: "updates", label: "Updates" },
  },
  {
    id: "tools",
    title: "Engines and tools",
    what: "Installing or updating a coding engine uses the npm registry. Node, LibreOffice, the voice helper, and voice models download from their publishers the first time a feature needs them.",
    off: "Each download runs only when a feature needs it. Engine versions are checked only when you ask, or when Engine versions is set to Latest.",
    section: { path: "coding-harnesses", label: "Coding engines" },
  },
  {
    id: "github",
    title: "GitHub",
    what: "For a repository with a GitHub remote, Tidebreak reads pull request state about once a minute with gh, fetches the base branch when you create a workspace, and loads avatars from GitHub.",
    off: "Remove the repository, or sign out of gh. Turn off Keep local main up to date to stop the fetch.",
    section: { path: "git-source-control", label: "Git & source control" },
  },
  {
    id: "gateway",
    title: "Model Gateway",
    what: "When you sign in, Tidebreak syncs your models and apps every five minutes and can send turns through the gateway.",
    off: "Sign out of the gateway.",
    section: { path: "gateway", label: "Model Gateway" },
  },
  {
    id: "mcp",
    title: "MCP servers and connected apps",
    what: "A remote MCP server receives the tool calls sent to it, and a health check every 15 seconds while it is on. A connected app calls its service when a tool uses it.",
    off: "Turn the server or app off.",
    section: { path: "connected-apps", label: "Connected apps" },
  },
  {
    id: "execution",
    title: "Code execution",
    what: "Code runs on this computer by default. E2B and Daytona receive the files a command works on.",
    off: "Choose Local.",
    section: { path: "code-execution", label: "Code execution" },
  },
  {
    id: "voice",
    title: "Voice input",
    what: "Voice is transcribed on this computer by default. The OpenAI and Gemini options send your recording to that provider.",
    off: "Choose Local.",
    section: { path: "voice-transcription", label: "Voice input" },
  },
  {
    id: "browser",
    title: "Browser and computer use",
    what: "Pages you or the agent open load from the web. Screenshots from computer use go to your model provider.",
    off: "Computer use needs the screen and accessibility permissions you grant. Remove them to stop it.",
    section: { path: "permissions", label: "Permissions" },
  },
  {
    id: "telemetry",
    title: "Telemetry",
    what: "Tidebreak sends no analytics, telemetry, or crash reports. Coding engines are separate programs with their own update checks and telemetry.",
    off: "Nothing to turn off in Tidebreak. Each engine's own settings control its telemetry.",
  },
];

/**
 * What Reset settings puts back to its defaults. The confirmation lists
 * exactly these, and the server's `POST /settings/reset` resets the first
 * group. Keep all three in step.
 */
export const RESET_SETTINGS_SCOPE = {
  resets: [
    "The default and background models, and new conversation defaults",
    "Context, background agent, and model list preferences, and prompt caching",
    "Coding engine options: turn recaps, closing rewrites, and engine versions",
    "Git branch naming",
    "On this computer: theme, zoom, notifications, and sidebar layout",
  ],
  keeps:
    "Conversations, memory, instructions, keys and providers, the Model Gateway, connected apps, web search, code execution, voice input, repositories, workspaces, and folder permissions stay as they are.",
} as const;

/** The preferences this window keeps in local storage that a reset clears. */
export const LOCAL_PREFERENCE_KEYS: readonly string[] = [
  THEME_KEY,
  ZOOM_KEY,
  FINISHED_KEY,
  NEEDS_YOU_KEY,
  SIDEBAR_COLLAPSED_KEY,
  SIDEBAR_WIDTH_KEY,
];

/** Clear this window's preferences. Storage that throws counts as cleared. */
export function clearLocalPreferences(storage: Storage | undefined): void {
  if (!storage) return;
  for (const key of LOCAL_PREFERENCE_KEYS) {
    try {
      storage.removeItem(key);
    } catch {
      // Blocked storage holds nothing to clear.
    }
  }
}

/** The file manager's name on this platform, for the button that opens it. */
export function revealLabel(userAgent: string): string {
  if (/Mac/i.test(userAgent)) return "Reveal in Finder";
  if (/Windows/i.test(userAgent)) return "Reveal in File Explorer";
  return "Open folder";
}
