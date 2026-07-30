import { FileSearch, ListChecks, MessageCircle, Sparkles } from "lucide-react";
import { Logomark } from "./Logomark";

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

export function WelcomeState({
  onSelectPrompt,
}: {
  onSelectPrompt?: (prompt: string) => void;
}) {
  return (
    <section className="welcome" aria-label="Start a chat">
      <span className="welcome-mark" aria-hidden="true">
        <Logomark />
      </span>
      <div className="welcome-copy">
        <h2>How can I help?</h2>
        <p>
          Ask a question, search sources attached to this chat, or start a task.
        </p>
      </div>
      {onSelectPrompt && (
        <div className="welcome-prompts">
          {STARTER_PROMPTS.map(({ icon: Icon, label, prompt }) => (
            <button
              key={label}
              type="button"
              className="flex items-center gap-2.5 rounded-[10px] border border-border bg-background px-3.5 py-2.5 text-[0.85rem] font-medium text-left text-foreground transition-[background-color,border-color] duration-[120ms] ease-in-out hover:border-[color-mix(in_srgb,var(--ink)_22%,var(--line))] hover:bg-accent [&_svg]:flex-none [&_svg]:text-muted-foreground [&:hover_svg]:text-foreground"
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
