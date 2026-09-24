import { QuitPrompt } from "./QuitPrompt";
import { useQuitPrompt } from "./desktopLifecycle";

/**
 * The quit prompt, mounted beside the app rather than inside it: a quit can
 * arrive while the shell is still booting or behind a gate, and the prompt
 * must still show. Being outside the router, it takes the way to the inbox
 * from the caller.
 */
export function DesktopQuitPromptHost({
  onOpenInbox,
}: {
  onOpenInbox?: () => void;
}) {
  const { update, answering, answer } = useQuitPrompt();
  return (
    <QuitPrompt
      prompt={update.prompt}
      error={update.error}
      restart={update.restart ?? false}
      answering={answering}
      onChoose={answer}
      onOpenInbox={onOpenInbox}
    />
  );
}
