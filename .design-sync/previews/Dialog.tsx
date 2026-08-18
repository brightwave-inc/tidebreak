import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Separator,
} from "tidebreak-desktop-ui";

export function RenameWorkspace() {
  return (
    <Dialog defaultOpen>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Rename workspace</DialogTitle>
          <DialogDescription>
            The branch keeps its name; only the workspace label in the sidebar changes.
          </DialogDescription>
        </DialogHeader>
        <Input defaultValue="Migrate settings schema (v3)" aria-label="Workspace name" />
        <DialogFooter>
          <Button variant="outline">Cancel</Button>
          <Button>Rename</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function ChatUsage() {
  return (
    <Dialog defaultOpen>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Chat usage</DialogTitle>
          <DialogDescription>
            Tokens billed for this chat since it was created. Cached input is charged at the
            provider's reduced rate.
          </DialogDescription>
        </DialogHeader>
        <div style={{ display: "flex", flexDirection: "column", gap: 8, fontSize: "0.875rem" }}>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span>Input</span>
            <span style={{ fontFamily: "var(--mono)" }}>412,880</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span>Cache read</span>
            <span style={{ fontFamily: "var(--mono)" }}>1,204,512</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span>Output</span>
            <span style={{ fontFamily: "var(--mono)" }}>38,104</span>
          </div>
          <Separator />
          <div style={{ display: "flex", justifyContent: "space-between", fontWeight: 600 }}>
            <span>Estimated cost</span>
            <span style={{ fontFamily: "var(--mono)" }}>$4.71</span>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline">Copy as JSON</Button>
          <Button>Done</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function NewWorkspace() {
  return (
    <Dialog defaultOpen>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New workspace</DialogTitle>
          <DialogDescription>
            Tidebreak branches from <span style={{ fontFamily: "var(--mono)" }}>main</span> and
            checks the branch out into its own directory.
          </DialogDescription>
        </DialogHeader>
        <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
          <Input defaultValue="tb/terminal-theme-tokens" aria-label="Branch name" />
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <Badge variant="secondary" size="sm">
              Claude Code
            </Badge>
            <Badge variant="secondary" size="sm">
              Ask before edits
            </Badge>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline">Cancel</Button>
          <Button>Create workspace</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
