import { Separator } from "tidebreak-desktop-ui";

export function SectionBreak() {
  return (
    <div style={{ maxWidth: "28rem", display: "flex", flexDirection: "column", gap: 12 }}>
      <div>
        <div style={{ fontWeight: 500 }}>Harness</div>
        <div style={{ fontSize: "0.875rem", color: "var(--muted-foreground)" }}>
          Claude Code drives this workspace. Approvals pause the turn.
        </div>
      </div>
      <Separator />
      <div>
        <div style={{ fontWeight: 500 }}>Branch protection</div>
        <div style={{ fontSize: "0.875rem", color: "var(--muted-foreground)" }}>
          Pushes go to <span style={{ fontFamily: "var(--mono)" }}>tb/*</span> branches; merges
          wait for green checks.
        </div>
      </div>
    </div>
  );
}

export function InlineMeta() {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        height: 20,
        fontSize: "0.875rem",
      }}
    >
      <span style={{ fontFamily: "var(--mono)" }}>tb/fix-retry-test</span>
      <Separator orientation="vertical" />
      <span>Claude Code</span>
      <Separator orientation="vertical" />
      <span>+128 −41</span>
      <Separator orientation="vertical" />
      <span style={{ color: "var(--muted-foreground)" }}>updated 2m ago</span>
    </div>
  );
}
