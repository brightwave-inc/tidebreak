import { Badge, ScrollArea, Separator } from "tidebreak-desktop-ui";

const changedFiles = [
  ["crates/tidebreak-server/src/turn.rs", "+48 −12"],
  ["crates/tidebreak-server/src/journal.rs", "+9 −3"],
  ["crates/tidebreak-server/src/session.rs", "+31 −18"],
  ["crates/tidebreak-server/src/registry.rs", "+6 −6"],
  ["crates/tidebreak-desktop/src/lib.rs", "+14 −2"],
  ["crates/tidebreak-desktop/ui/src/MessageList.tsx", "+21 −7"],
  ["crates/tidebreak-desktop/ui/src/ToolCallCard.tsx", "+11 −4"],
  ["crates/tidebreak-desktop/ui/src/code/CodeTranscript.tsx", "+38 −22"],
  ["crates/tidebreak-desktop/ui/src/sidebar/primitives.tsx", "+5 −1"],
  ["docs/model-providers.md", "+12 −0"],
  ["docs/releases.md", "+3 −3"],
  ["Cargo.lock", "+42 −40"],
  [".github/workflows/ci.yml", "+8 −2"],
  ["crates/tidebreak-server/tests/turn_state.rs", "+56 −0"],
];

export function ChangedFiles() {
  return (
    <ScrollArea
      type="always"
      className="rounded-md border"
      style={{ height: 220, width: 460 }}
    >
      <div
        style={{
          width: 446,
          boxSizing: "border-box",
          padding: 10,
          display: "flex",
          flexDirection: "column",
          gap: 7,
        }}
      >
        {changedFiles.map(([path, diff]) => (
          <div
            key={path}
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              gap: 12,
              fontSize: 12,
            }}
          >
            <span
              style={{
                fontFamily: "var(--mono)",
                minWidth: 0,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {path}
            </span>
            <span style={{ color: "var(--muted-foreground)", whiteSpace: "nowrap" }}>{diff}</span>
          </div>
        ))}
      </div>
    </ScrollArea>
  );
}

export function PRChecks() {
  const checks: Array<[string, "success" | "critical" | "info", string]> = [
    ["rustfmt", "success", "passed"],
    ["clippy --all-targets", "success", "passed"],
    ["desktop tests", "success", "passed"],
    ["workspace tests", "info", "running"],
    ["postgres turn-state", "info", "running"],
    ["ui pnpm test", "success", "passed"],
    ["ui pnpm build", "success", "passed"],
    ["windows native", "critical", "failed"],
    ["release dry run", "success", "passed"],
  ];
  return (
    <ScrollArea
      type="always"
      className="rounded-md border"
      style={{ height: 180, width: 340 }}
    >
      <div style={{ padding: 10, display: "flex", flexDirection: "column" }}>
        {checks.map(([name, variant, label], i) => (
          <div key={name}>
            {i > 0 && <Separator className="my-2" />}
            <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 13 }}>
              <Badge variant={variant} size="sm">
                {label}
              </Badge>
              <span style={{ fontFamily: "var(--mono)", fontSize: 12 }}>{name}</span>
            </div>
          </div>
        ))}
      </div>
    </ScrollArea>
  );
}

export function HorizontalLog() {
  return (
    <ScrollArea
      type="always"
      className="rounded-md border bg-muted/30"
      style={{ width: 460, height: 108 }}
    >
      <pre
        style={{
          fontFamily: "var(--mono)",
          fontSize: 12,
          lineHeight: 1.6,
          margin: 0,
          padding: "10px 10px 16px",
          whiteSpace: "pre",
        }}
      >
        {`$ cargo clippy --workspace --all-targets --locked -- -D warnings
warning: unused variable: \`retry_budget\` --> crates/tidebreak-server/src/turn.rs:118:9
error: this \`if\` statement can be collapsed --> crates/tidebreak-server/src/session.rs:204:5
error: could not compile \`tidebreak-server\` (lib) due to 1 previous error; 1 warning emitted`}
      </pre>
    </ScrollArea>
  );
}
