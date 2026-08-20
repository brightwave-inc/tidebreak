import { useEffect, useState } from "react";
import {
  AppWindow,
  ArrowUpRight,
  GitBranch,
  Globe,
  Scale,
  Sparkles,
} from "lucide-react";
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
import { Button } from "@/components/ui/button";

/**
 * What the welcome screen calls on the prompt library.
 *
 * The library's own two calls, narrowed: the catalog names the prompts, and
 * one body is fetched only when a card is picked.
 */
export type PromptLibraryApis = Pick<PluginsApis, "list" | "promptBody">;

/** How many library prompts home offers before it stops listing them. */
const MAX_LIBRARY_PROMPTS = 6;

/** Optional setup applied when a built-in starter is picked. */
export type StarterPromptOptions = {
  enableInternet?: boolean;
};

type StarterPrompt = {
  icon: typeof Sparkles;
  label: string;
  description: string;
  prompt: string;
  enableInternet?: boolean;
};

/**
 * The starters home falls back to with no prompt library installed.
 *
 * Each prompt is complete: it names the work, the tools to use, and the
 * finished artifact, so a card that is clicked and sent starts a turn the
 * reader can watch without attaching files or answering a follow-up.
 */
const STARTER_PROMPTS: StarterPrompt[] = [
  {
    icon: Globe,
    label: "Brief this week's AI news",
    description: "Search the web, then write a sourced briefing.",
    prompt:
      "Search the web for the most important AI model and product news from the past seven days. Write a one-page briefing with sources. Cover what shipped, why it matters, and what to watch next. Start now. Do not ask me what to include.",
    enableInternet: true,
  },
  {
    icon: Scale,
    label: "Compare two public products",
    description: "Search, tabulate, and recommend with citations.",
    prompt:
      "Search the web for current public pricing and specs for a cloud NVIDIA H100 versus H200 GPU instance from at least two providers. Build a comparison table covering price, memory, availability notes, and who each is for. Recommend when to pick which, and cite sources. Start now. Do not ask me for more requirements.",
    enableInternet: true,
  },
  {
    icon: AppWindow,
    label: "Build a local planner",
    description: "Create a small app in this workspace.",
    prompt:
      "Build a small local web app I can open on this machine: a personal weekly planner with add, edit, and complete for tasks. No login, single page, clean UI. Create the files in this workspace and tell me how to open it. Start now. Do not wait for a folder or more product direction.",
  },
  {
    icon: GitBranch,
    label: "Research in parallel",
    description: "Split a briefing across background tasks.",
    prompt:
      "Search the web and split this into parallel background tasks: (1) EU AI Act enforcement developments this year, (2) US state AI bills that passed or advanced this year, (3) policy responses published by major AI labs. Synthesize a one-page brief with sources. Start now. Do not ask me to pick a region or angle.",
    enableInternet: true,
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
  "group flex min-h-[5.25rem] items-start gap-3 rounded-xl bg-muted/35 px-4 py-3.5 text-left text-sm text-foreground ring-1 ring-inset ring-border-subtle transition-[transform,background-color,box-shadow] duration-200 hover:-translate-y-0.5 hover:bg-muted/60 hover:shadow-sm focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/25 active:translate-y-0";

function PromptCard({
  icon: Icon,
  label,
  description,
  first,
  onClick,
}: {
  icon: typeof Sparkles;
  label: string;
  description?: string;
  first: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={CARD_CLASS}
      data-first-task-target={first ? "starter-choice" : undefined}
      onClick={onClick}
    >
      <span className="grid size-8 shrink-0 place-items-center rounded-lg bg-background text-muted-foreground shadow-[inset_0_0_0_1px_var(--border-subtle)] transition-colors duration-200 group-hover:text-foreground">
        <Icon size={17} strokeWidth={1.75} />
      </span>
      <span className="min-w-0 flex-1 pt-0.5">
        <span className="block font-medium tracking-[-0.01em]">{label}</span>
        {description && (
          <span className="mt-0.5 block text-[0.8rem] leading-5 font-normal text-muted-foreground">
            {description}
          </span>
        )}
      </span>
      <ArrowUpRight
        aria-hidden="true"
        className="mt-1 size-4 shrink-0 text-muted-foreground/45 transition-[color,transform] duration-200 group-hover:translate-x-0.5 group-hover:-translate-y-0.5 group-hover:text-foreground"
      />
    </button>
  );
}

export function WelcomeState({
  onSelectPrompt,
  executionConfigClient,
  promptLibrary,
  heading = "How can I help?",
  description = "Ask a question, work through your files, or start a task.",
  onStartWalkthrough,
}: {
  onSelectPrompt?: (prompt: string, options?: StarterPromptOptions) => void;
  executionConfigClient?: Pick<ApiClient, "getCodeExecutionConfig">;
  heading?: string;
  description?: string;
  onStartWalkthrough?: () => void;
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
    <section className="welcome" aria-label="Start work">
      <div className="welcome-intro">
        <span className="welcome-mark" aria-hidden="true">
          <Logomark />
        </span>
        <div className="welcome-copy">
          <h2>{heading}</h2>
          <p>{description}</p>
          {managedExecutionOnly && (
            <p className="welcome-disclosure">
              {MANAGED_EXECUTION_DISCLOSURE}
            </p>
          )}
        </div>
      </div>
      {onStartWalkthrough && (
        <Button
          className="self-center"
          type="button"
          size="sm"
          onClick={onStartWalkthrough}
        >
          Set up your first task
        </Button>
      )}
      {onSelectPrompt && (
        <div className="welcome-prompts" data-first-task-target="starters">
          {libraryPrompts.length > 0
            ? libraryPrompts.map((prompt, index) => (
                <PromptCard
                  key={prompt.name}
                  icon={Sparkles}
                  label={promptTitle(prompt.name)}
                  description={prompt.description}
                  first={index === 0}
                  onClick={() => insertLibraryPrompt(prompt.name)}
                />
              ))
            : STARTER_PROMPTS.map(
                (
                  {
                    icon,
                    label,
                    description,
                    prompt,
                    enableInternet,
                  },
                  index,
                ) => (
                  <PromptCard
                    key={label}
                    icon={icon}
                    label={label}
                    description={description}
                    first={index === 0}
                    onClick={() =>
                      onSelectPrompt(
                        prompt,
                        enableInternet ? { enableInternet: true } : undefined,
                      )
                    }
                  />
                ),
              )}
        </div>
      )}
    </section>
  );
}
