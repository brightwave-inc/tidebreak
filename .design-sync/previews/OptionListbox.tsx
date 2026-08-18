import { OptionListbox } from "tidebreak-desktop-ui";
import { AtSign, BookOpen, FileCode2, GitBranch, Puzzle, Terminal } from "lucide-react";

const noop = () => {};

export function SlashPlugins() {
  return (
    <div
      className="rounded-md border border-border bg-popover text-popover-foreground shadow-md"
      style={{ maxWidth: 380 }}
    >
      <OptionListbox
        listId="preview-slash"
        label="Plugin library"
        activeIndex={0}
        onPick={noop}
        onHighlight={noop}
        rows={[
          {
            key: "skill:code-review",
            label: "/code-review",
            description: "Review the current diff for correctness bugs",
            icon: BookOpen,
            hint: "Skill",
          },
          {
            key: "skill:security-review",
            label: "/security-review",
            description: "Audit the branch for secrets and unsafe calls",
            icon: BookOpen,
            hint: "Skill",
          },
          {
            key: "prompt:release-notes",
            label: "/release-notes",
            description: "Draft notes from the merged pull requests",
            icon: Puzzle,
            hint: "Prompt",
          },
        ]}
      />
    </div>
  );
}

export function WithDisabledAndNote() {
  return (
    <div
      className="rounded-md border border-border bg-popover text-popover-foreground shadow-md"
      style={{ maxWidth: 380 }}
    >
      <OptionListbox
        listId="preview-capped"
        label="Plugin library"
        activeIndex={1}
        note="A message can invoke at most 3 skills."
        onPick={noop}
        onHighlight={noop}
        rows={[
          {
            key: "skill:code-review",
            label: "/code-review",
            description: "Review the current diff for correctness bugs",
            icon: BookOpen,
            hint: "Skill",
          },
          {
            key: "skill:run",
            label: "/run",
            description: "Launch the desktop app and drive the change",
            icon: Terminal,
            hint: "Skill",
          },
          {
            key: "skill:simplify",
            label: "/simplify",
            description: "Collapse the duplicated retry helpers",
            icon: BookOpen,
            hint: "Cap reached",
            disabled: true,
          },
        ]}
      />
    </div>
  );
}

export function MentionTargets() {
  return (
    <div
      className="rounded-md border border-border bg-popover text-popover-foreground shadow-md"
      style={{ maxWidth: 380 }}
    >
      <OptionListbox
        listId="preview-mentions"
        label="Mention a workspace or file"
        activeIndex={2}
        onPick={noop}
        onHighlight={noop}
        rows={[
          {
            key: "ws:fix-retry-test",
            label: "Fix flaky retry test",
            description: "tb/fix-retry-test · Claude Code",
            icon: GitBranch,
            hint: "Workspace",
          },
          {
            key: "ws:settings-schema",
            label: "Migrate settings schema",
            description: "tb/settings-schema · Codex",
            icon: GitBranch,
            hint: "Workspace",
          },
          {
            key: "file:turn-state",
            label: "turn_state.rs",
            description: "crates/tidebreak-server/src/turn_state.rs",
            icon: FileCode2,
            hint: "File",
          },
          {
            key: "file:composer",
            label: "Composer.tsx",
            description: "crates/tidebreak-desktop/ui/src/Composer.tsx",
            icon: AtSign,
            hint: "File",
          },
        ]}
      />
    </div>
  );
}
