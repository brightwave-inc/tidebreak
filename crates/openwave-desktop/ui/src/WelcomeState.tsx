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
    label: "Search my documents",
    prompt: "Search my connected documents for ",
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
    <section className="welcome" aria-label="Start a conversation">
      <span className="welcome-mark" aria-hidden="true">
        <Logomark />
      </span>
      <div className="welcome-copy">
        <h2>How can I help?</h2>
        <p>
          Ask a question, search your connected documents, or start a task.
        </p>
      </div>
      {onSelectPrompt && (
        <div className="welcome-prompts">
          {STARTER_PROMPTS.map(({ icon: Icon, label, prompt }) => (
            <button
              key={label}
              type="button"
              className="welcome-prompt"
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
