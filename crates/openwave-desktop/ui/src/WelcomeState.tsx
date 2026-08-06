import { useEffect, useState } from "react";
import { FileSearch, ListChecks, MessageCircle, Sparkles } from "lucide-react";
import type {
  ApiClient,
  CodeExecutionConfigInfo,
  PluginPromptInfo,
} from "./api";
import {
  MANAGED_EXECUTION_DISCLOSURE,
  requiresManagedExecutionDisclosure,
} from "./CodeExecutionDisclosure";
import { Logomark } from "./Logomark";
import type { PluginsApis } from "./plugins/pluginsApis";

/**
 * What the welcome screen calls on the prompt library.
 *
 * The library's own two calls, narrowed: the catalog names the prompts, and
 * one body is fetched only when a card is picked.
 */
export type PromptLibraryApis = Pick<PluginsApis, "list" | "promptBody">;

/** How many library prompts home offers before it stops listing them. */
const MAX_LIBRARY_PROMPTS = 6;

type StarterPrompt = {
  icon: typeof Sparkles;
  label: string;
  prompt: string;
};

const STARTER_PROMPTS: StarterPrompt[] = [
  {
    icon: MessageCircle,
    label: "What can you help me with?",
    prompt: "What can you help me with?",
  },
  {
    icon: FileSearch,
    label: "Search this chat's sources",
    prompt: "Search the sources in this chat for ",
  },
  {
    icon: Sparkles,
    label: "Summarize a document",
    prompt: "Summarize the key points from ",
  },
  {
    icon: ListChecks,
    label: "Draft a plan",
    prompt: "Help me draft a plan for ",
  },
];

/**
 * A prompt slug as a card title: `weekly-update` reads "Weekly update".
 *
 * The package's own description is the tip below it, so the title only has to
 * be a legible name rather than a summary.
 */
export function promptTitle(name: string): string {
  const words = name.replace(/[-_]+/g, " ").trim();
  if (!words) return name;
  return words.charAt(0).toUpperCase() + words.slice(1);
}

const CARD_CLASS =
  "flex items-center gap-2.5 rounded-[10px] border border-border bg-background px-3.5 py-2.5 text-[0.85rem] font-medium text-left text-foreground transition-[background-color,border-color] duration-[120ms] ease-in-out hover:border-[color-mix(in_srgb,var(--ink)_22%,var(--line))] hover:bg-accent [&_svg]:flex-none [&_svg]:text-muted-foreground [&:hover_svg]:text-foreground";

export function WelcomeState({
  onSelectPrompt,
  executionConfigClient,
  promptLibrary,
}: {
  onSelectPrompt?: (prompt: string) => void;
  executionConfigClient?: Pick<ApiClient, "getCodeExecutionConfig">;
  /**
   * The installed prompt library, when this surface offers it. Absent — or
   * empty, or still loading — the hardcoded starters stand, so an install with
   * no prompts sees exactly what it saw before.
   */
  promptLibrary?: PromptLibraryApis;
}) {
  const [executionProviders, setExecutionProviders] = useState<
    CodeExecutionConfigInfo["providers"] | null
  >(null);
  const [libraryPrompts, setLibraryPrompts] = useState<PluginPromptInfo[]>([]);

  useEffect(() => {
    if (
      !executionConfigClient ||
      typeof executionConfigClient.getCodeExecutionConfig !== "function"
    ) {
      return;
    }
    let cancelled = false;
    setExecutionProviders(null);
    void executionConfigClient
      .getCodeExecutionConfig()
      .then((config) => {
        if (!cancelled) setExecutionProviders(config.providers);
      })
      // A missing disclosure is safer than inventing platform facts when the
      // capability report cannot be read.
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [executionConfigClient]);

  useEffect(() => {
    if (!promptLibrary) return;
    let cancelled = false;
    void (async () => {
      try {
        const catalog = await promptLibrary.list();
        if (cancelled) return;
        setLibraryPrompts(
          catalog.prompts
            .filter((prompt) => prompt.enabled)
            .slice(0, MAX_LIBRARY_PROMPTS),
        );
      } catch {
        // A library that cannot be read is a library with nothing to offer:
        // home keeps its starters rather than reporting a failure nobody
        // asked for.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [promptLibrary]);

  const managedExecutionOnly = requiresManagedExecutionDisclosure(
    executionProviders,
  );

  function insertLibraryPrompt(name: string) {
    if (!onSelectPrompt || !promptLibrary) return;
    void (async () => {
      try {
        const body = await promptLibrary.promptBody(name);
        onSelectPrompt(body.body);
      } catch {
        // Picking is not a place to raise an error: a body that cannot be
        // read simply leaves the composer as it was.
      }
    })();
  }

  return (
    <section className="welcome" aria-label="Start a chat">
      <span className="welcome-mark" aria-hidden="true">
        <Logomark />
      </span>
      <div className="welcome-copy">
        <h2>How can I help?</h2>
        <p>
          Ask a question, search attached sources, or start a task.
        </p>
        {managedExecutionOnly && <p>{MANAGED_EXECUTION_DISCLOSURE}</p>}
      </div>
      {onSelectPrompt && libraryPrompts.length > 0 && (
        <div className="welcome-prompts">
          {libraryPrompts.map((prompt) => (
            <button
              key={prompt.name}
              type="button"
              className={CARD_CLASS}
              onClick={() => insertLibraryPrompt(prompt.name)}
            >
              <Sparkles size={16} />
              <span className="min-w-0">
                <span className="block">{promptTitle(prompt.name)}</span>
                {prompt.description && (
                  <span className="block text-[0.78rem] font-normal text-muted-foreground">
                    {prompt.description}
                  </span>
                )}
              </span>
            </button>
          ))}
        </div>
      )}
      {onSelectPrompt && libraryPrompts.length === 0 && (
        <div className="welcome-prompts">
          {STARTER_PROMPTS.map(({ icon: Icon, label, prompt }) => (
            <button
              key={label}
              type="button"
              className={CARD_CLASS}
              onClick={() => onSelectPrompt(prompt)}
            >
              <Icon size={16} />
              <span>{label}</span>
            </button>
          ))}
        </div>
      )}
    </section>
  );
}
