import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "tidebreak-desktop-ui";

export function DiscardChanges() {
  return (
    <AlertDialog defaultOpen>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Discard uncommitted changes?</AlertDialogTitle>
          <AlertDialogDescription>
            The workspace on <span style={{ fontFamily: "var(--mono)" }}>tb/terminal-theme</span>{" "}
            has 3 modified files that were never committed. Discarding removes them from the
            checkout; the agent's transcript is kept.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Keep changes</AlertDialogCancel>
          <AlertDialogAction variant="destructive">Discard changes</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

export function StopRunningAgents() {
  return (
    <AlertDialog defaultOpen>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Stop 2 running agents?</AlertDialogTitle>
          <AlertDialogDescription>
            Claude Code is mid-turn on two workspaces. Stopping cancels the turns in flight —
            files already written stay on their branches.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Let them finish</AlertDialogCancel>
          <AlertDialogAction>Stop agents</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

export function DeleteRepository() {
  return (
    <AlertDialog defaultOpen>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Remove the scheduler repo from Tidebreak?</AlertDialogTitle>
          <AlertDialogDescription>
            This removes the project, its 4 workspaces, and every transcript Tidebreak holds for
            them. The clone on disk and the remote are untouched.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction variant="destructive">Remove project</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
