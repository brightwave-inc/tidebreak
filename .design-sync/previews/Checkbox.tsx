import { Checkbox } from "tidebreak-desktop-ui";

export function States() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <label className="flex items-center gap-2 text-sm">
        <Checkbox />
        Delete the branch after merge
      </label>
      <label className="flex items-center gap-2 text-sm">
        <Checkbox checked />
        Enable auto-merge when checks pass
      </label>
      <label className="flex items-center gap-2 text-sm">
        <Checkbox checked="indeterminate" />
        Stage all changed files
      </label>
      <label className="flex items-center gap-2 text-sm text-muted-foreground">
        <Checkbox disabled />
        Sign commits (no signing key configured)
      </label>
      <label className="flex items-center gap-2 text-sm text-muted-foreground">
        <Checkbox disabled checked />
        Run CI on push (required by branch protection)
      </label>
    </div>
  );
}

export function HarnessFilter() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8, maxWidth: 260 }}>
      <label className="flex items-center gap-2 text-sm">
        <Checkbox checked />
        <span className="flex-1">Claude Code</span>
        <span className="text-xs text-muted-foreground">12</span>
      </label>
      <label className="flex items-center gap-2 text-sm">
        <Checkbox checked />
        <span className="flex-1">Codex</span>
        <span className="text-xs text-muted-foreground">4</span>
      </label>
      <label className="flex items-center gap-2 text-sm">
        <Checkbox />
        <span className="flex-1">OpenCode</span>
        <span className="text-xs text-muted-foreground">1</span>
      </label>
    </div>
  );
}
