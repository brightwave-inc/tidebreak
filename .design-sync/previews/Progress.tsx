import { Progress } from "tidebreak-desktop-ui";

export function Values() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 14, width: 360 }}>
      <div>
        <div style={{ fontSize: 12, color: "var(--muted-foreground)", marginBottom: 6 }}>
          Cloning repository — 0%
        </div>
        <Progress value={0} />
      </div>
      <div>
        <div style={{ fontSize: 12, color: "var(--muted-foreground)", marginBottom: 6 }}>
          Indexing workspace — 35%
        </div>
        <Progress value={35} />
      </div>
      <div>
        <div style={{ fontSize: 12, color: "var(--muted-foreground)", marginBottom: 6 }}>
          Running checks — 70%
        </div>
        <Progress value={70} />
      </div>
      <div>
        <div style={{ fontSize: 12, color: "var(--muted-foreground)", marginBottom: 6 }}>
          Build complete
        </div>
        <Progress value={100} />
      </div>
    </div>
  );
}

export function ContextUsage() {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 10, width: 320 }}>
      <Progress value={66} style={{ width: 140 }} className="h-1.5" />
      <span style={{ fontSize: 12, color: "var(--muted-foreground)", whiteSpace: "nowrap" }}>
        132k / 200k tokens
      </span>
    </div>
  );
}

export function TaskPlan() {
  return (
    <div style={{ width: 360 }}>
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          fontSize: 13,
          marginBottom: 6,
        }}
      >
        <span>Migrate settings schema</span>
        <span style={{ color: "var(--muted-foreground)" }}>3 of 5 tasks</span>
      </div>
      <Progress value={60} />
    </div>
  );
}
