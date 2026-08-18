import { Badge, Tabs, TabsContent, TabsList, TabsTrigger } from "tidebreak-desktop-ui";

export function PanelTabs() {
  return (
    <Tabs defaultValue="code" style={{ width: 420 }}>
      <TabsList className="flex w-full items-center justify-start gap-1 border-b px-1">
        <TabsTrigger value="chat">Chat</TabsTrigger>
        <TabsTrigger value="code">Code</TabsTrigger>
        <TabsTrigger value="terminal">Terminal</TabsTrigger>
      </TabsList>
      <TabsContent value="code">
        <div style={{ display: "flex", flexDirection: "column", gap: 6, fontSize: 13 }}>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ fontFamily: "var(--mono)" }}>crates/tidebreak-server/src/turn.rs</span>
            <span style={{ color: "var(--muted-foreground)" }}>+48 −12</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ fontFamily: "var(--mono)" }}>crates/tidebreak-server/src/journal.rs</span>
            <span style={{ color: "var(--muted-foreground)" }}>+9 −3</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ fontFamily: "var(--mono)" }}>ui/src/MessageList.tsx</span>
            <span style={{ color: "var(--muted-foreground)" }}>+21 −7</span>
          </div>
        </div>
      </TabsContent>
    </Tabs>
  );
}

export function CommandOutput() {
  return (
    <Tabs defaultValue="output" style={{ width: 420 }}>
      <TabsList className="flex w-full items-center justify-start gap-1 border-b px-1">
        <TabsTrigger value="command" className="py-1 text-xs capitalize">
          Command
        </TabsTrigger>
        <TabsTrigger value="output" className="py-1 text-xs capitalize">
          Output
        </TabsTrigger>
      </TabsList>
      <TabsContent value="output" className="mt-0">
        <pre
          style={{
            fontFamily: "var(--mono)",
            fontSize: 12,
            lineHeight: 1.6,
            margin: 0,
            paddingTop: 8,
          }}
        >
          {`running 3 tests
test turn::retries_transient_failure ... ok
test turn::caps_retry_budget ... ok
test turn::journal_records_attempts ... ok

test result: ok. 3 passed; 0 failed`}
        </pre>
      </TabsContent>
    </Tabs>
  );
}

export function WithDisabled() {
  return (
    <Tabs defaultValue="checks" style={{ width: 420 }}>
      <TabsList className="flex w-full items-center justify-start gap-1 border-b px-1">
        <TabsTrigger value="checks">Checks</TabsTrigger>
        <TabsTrigger value="commits">Commits</TabsTrigger>
        <TabsTrigger value="diff" disabled>
          Diff
        </TabsTrigger>
      </TabsList>
      <TabsContent value="checks">
        <div style={{ display: "flex", flexDirection: "column", gap: 8, fontSize: 13 }}>
          <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
            <Badge variant="success" size="sm">
              passed
            </Badge>
            <span>rustfmt</span>
          </div>
          <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
            <Badge variant="info" size="sm">
              running
            </Badge>
            <span>clippy --all-targets</span>
          </div>
          <div style={{ fontSize: 12, color: "var(--muted-foreground)" }}>
            Diff unavailable — no changes pushed yet.
          </div>
        </div>
      </TabsContent>
    </Tabs>
  );
}
