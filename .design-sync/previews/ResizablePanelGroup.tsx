import {
  Badge,
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "tidebreak-desktop-ui";

function PanelBody({ title, children }: { title: string; children?: React.ReactNode }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      <div
        style={{
          padding: "8px 12px",
          borderBottom: "1px solid var(--border)",
          fontSize: 13,
          fontWeight: 500,
        }}
      >
        {title}
      </div>
      <div style={{ padding: 12, fontSize: 12, overflow: "hidden" }}>{children}</div>
    </div>
  );
}

export function ChatAndCode() {
  return (
    <div style={{ height: 240, border: "1px solid var(--border)", borderRadius: 8 }}>
      <ResizablePanelGroup direction="horizontal">
        <ResizablePanel defaultSize={55} minSize={30}>
          <PanelBody title="Chat — Fix flaky retry test">
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <div style={{ color: "var(--muted-foreground)" }}>
                You: The retry test fails about one run in five on CI.
              </div>
              <div>
                Claude: The retry loop never records its attempt count, so the
                journal replay asserts against a stale value. I added the budget
                guard and a journal entry per attempt.
              </div>
            </div>
          </PanelBody>
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel defaultSize={45} minSize={25}>
          <PanelBody title="Changed files">
            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <div style={{ display: "flex", justifyContent: "space-between" }}>
                <span style={{ fontFamily: "var(--mono)" }}>src/turn.rs</span>
                <span style={{ color: "var(--muted-foreground)" }}>+48 −12</span>
              </div>
              <div style={{ display: "flex", justifyContent: "space-between" }}>
                <span style={{ fontFamily: "var(--mono)" }}>src/journal.rs</span>
                <span style={{ color: "var(--muted-foreground)" }}>+9 −3</span>
              </div>
              <div style={{ display: "flex", justifyContent: "space-between" }}>
                <span style={{ fontFamily: "var(--mono)" }}>tests/turn_state.rs</span>
                <span style={{ color: "var(--muted-foreground)" }}>+56 −0</span>
              </div>
            </div>
          </PanelBody>
        </ResizablePanel>
      </ResizablePanelGroup>
    </div>
  );
}

export function ThreeColumn() {
  return (
    <div style={{ height: 240, border: "1px solid var(--border)", borderRadius: 8 }}>
      <ResizablePanelGroup direction="horizontal">
        <ResizablePanel defaultSize={22} minSize={15}>
          <PanelBody title="Workspaces">
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
                <span>Fix flaky retry test</span>
              </div>
              <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
                <span>Settings schema</span>
                <Badge variant="info" size="sm">
                  running
                </Badge>
              </div>
              <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
                <span>Terminal theme</span>
                <Badge variant="warning" size="sm">
                  needs you
                </Badge>
              </div>
            </div>
          </PanelBody>
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel defaultSize={48} minSize={30}>
          <PanelBody title="Transcript">
            <div style={{ color: "var(--muted-foreground)" }}>
              Running cargo test -p tidebreak-server --locked…
            </div>
            <div style={{ marginTop: 8 }}>
              18 tests passed. Pushing branch tb/fix-retry-test.
            </div>
          </PanelBody>
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel defaultSize={30} minSize={20}>
          <PanelBody title="Review">
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
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
                <span>clippy</span>
              </div>
            </div>
          </PanelBody>
        </ResizablePanel>
      </ResizablePanelGroup>
    </div>
  );
}

export function VerticalSplit() {
  return (
    <div style={{ height: 280, border: "1px solid var(--border)", borderRadius: 8 }}>
      <ResizablePanelGroup direction="vertical">
        <ResizablePanel defaultSize={60} minSize={30}>
          <PanelBody title="src/turn.rs">
            <pre
              style={{
                fontFamily: "var(--mono)",
                fontSize: 12,
                lineHeight: 1.6,
                margin: 0,
              }}
            >
              {`fn retry(&mut self) -> Result<()> {
    if self.attempts >= self.budget {
        return Err(Error::RetryBudgetExhausted);
    }
    self.attempts += 1;
    self.run()
}`}
            </pre>
          </PanelBody>
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel defaultSize={40} minSize={20}>
          <PanelBody title="Terminal">
            <pre
              style={{
                fontFamily: "var(--mono)",
                fontSize: 12,
                lineHeight: 1.6,
                margin: 0,
              }}
            >
              {`$ cargo test -p tidebreak-server --locked
test result: ok. 18 passed; 0 failed`}
            </pre>
          </PanelBody>
        </ResizablePanel>
      </ResizablePanelGroup>
    </div>
  );
}
