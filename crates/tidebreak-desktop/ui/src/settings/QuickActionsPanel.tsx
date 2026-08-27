import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import {
  useWorkflowPromptStore,
  workflowPromptDraft,
  workflowPromptIsCustom,
  WORKFLOW_PROMPT_FIELDS,
} from "@/code/workflowPrompts";
import { SettingsField, SettingsPanel, SettingsSection } from "./primitives";

/**
 * The prompts workspace actions send into chat.
 *
 * Create PR, Fix CI, and the other prompt actions submit this text rather
 * than leaving it in the composer. Each field saves as you type; Reset
 * restores the shipped wording for that action only.
 */
export function QuickActionsPanel() {
  const overrides = useWorkflowPromptStore((state) => state.overrides);
  const setPrompt = useWorkflowPromptStore((state) => state.setPrompt);
  const resetPrompt = useWorkflowPromptStore((state) => state.resetPrompt);

  return (
    <SettingsPanel
      title="Quick actions"
      description="The prompts Tidebreak sends when you click a workspace action such as Create PR. Each one goes into the workspace chat as soon as you click it. {base} is the target branch. {pr} is the pull request number."
    >
      <SettingsSection>
        {WORKFLOW_PROMPT_FIELDS.map((field) => {
          const customized = workflowPromptIsCustom(field.id, overrides);
          return (
            <div key={field.id} className="flex flex-col gap-2">
              <SettingsField label={field.label} hint={field.hint}>
                <Textarea
                  value={workflowPromptDraft(field.id, overrides)}
                  onChange={(event) => setPrompt(field.id, event.target.value)}
                  rows={4}
                  spellCheck
                  aria-label={`${field.label} prompt`}
                />
              </SettingsField>
              {customized && (
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  className="self-start px-0 text-muted-foreground hover:bg-transparent"
                  aria-label={`Reset ${field.label} to default`}
                  onClick={() => resetPrompt(field.id)}
                >
                  Reset to default
                </Button>
              )}
            </div>
          );
        })}
      </SettingsSection>
    </SettingsPanel>
  );
}
