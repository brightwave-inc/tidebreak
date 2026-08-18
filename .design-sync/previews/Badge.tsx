import { Badge } from "tidebreak-desktop-ui";

export function Variants() {
  return (
    <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
      <Badge>main</Badge>
      <Badge variant="secondary">Draft</Badge>
      <Badge variant="destructive">Rebase failed</Badge>
      <Badge variant="outline">3 files</Badge>
      <Badge variant="success">Checks passed</Badge>
      <Badge variant="warning">Needs you</Badge>
      <Badge variant="critical">Conflict</Badge>
      <Badge variant="info">Running</Badge>
    </div>
  );
}

export function Small() {
  return (
    <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
      <Badge size="sm">v0.4.2</Badge>
      <Badge variant="secondary" size="sm">
        Queued
      </Badge>
      <Badge variant="destructive" size="sm">
        Lint failed
      </Badge>
      <Badge variant="outline" size="sm">
        +128 −41
      </Badge>
      <Badge variant="success" size="sm">
        PR open
      </Badge>
      <Badge variant="warning" size="sm">
        Approval pending
      </Badge>
      <Badge variant="critical" size="sm">
        Merge conflict
      </Badge>
      <Badge variant="info" size="sm">
        Building
      </Badge>
    </div>
  );
}

export function InContext() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10, maxWidth: 440 }}>
      <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
        <span style={{ fontSize: 14 }}>Fix flaky retry test</span>
        <span style={{ fontFamily: "var(--mono)", fontSize: 12, color: "var(--muted-foreground)" }}>
          tb/fix-retry-test
        </span>
        <Badge variant="success" size="sm">
          PR open
        </Badge>
      </div>
      <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
        <span style={{ fontSize: 14 }}>Migrate settings schema</span>
        <span style={{ fontFamily: "var(--mono)", fontSize: 12, color: "var(--muted-foreground)" }}>
          tb/settings-schema
        </span>
        <Badge variant="info" size="sm">
          Running
        </Badge>
        <Badge variant="outline" size="sm">
          +64 −12
        </Badge>
      </div>
      <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
        <span style={{ fontSize: 14 }}>Terminal theme tokens</span>
        <span style={{ fontFamily: "var(--mono)", fontSize: 12, color: "var(--muted-foreground)" }}>
          tb/terminal-theme
        </span>
        <Badge variant="warning" size="sm">
          Needs you
        </Badge>
      </div>
    </div>
  );
}
