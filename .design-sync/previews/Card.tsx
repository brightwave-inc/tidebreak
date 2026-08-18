import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "tidebreak-desktop-ui";

export function WorkspaceCard() {
  return (
    <Card style={{ maxWidth: "28rem", border: "1px solid var(--border)" }}>
      <CardHeader>
        <CardTitle>Fix flaky retry test</CardTitle>
        <Badge variant="success" size="sm">
          PR open
        </Badge>
      </CardHeader>
      <CardContent>
        <CardDescription>
          Claude Code on <span style={{ fontFamily: "var(--mono)" }}>tb/fix-retry-test</span> —
          registered the timer before yielding to the executor, then re-ran the suite 500 times
          without a failure.
        </CardDescription>
      </CardContent>
      <CardFooter>
        <div style={{ display: "flex", alignItems: "center", gap: 12, width: "100%" }}>
          <span>PR #2183 · +128 −41</span>
          <div style={{ marginLeft: "auto", display: "flex", gap: 8 }}>
            <Button variant="outline" size="sm">
              View checks
            </Button>
            <Button size="sm">Open pull request</Button>
          </div>
        </div>
      </CardFooter>
    </Card>
  );
}

export function SessionSummary() {
  return (
    <Card style={{ maxWidth: "24rem", border: "1px solid var(--border)" }}>
      <CardHeader>
        <CardTitle>Migrate settings schema</CardTitle>
      </CardHeader>
      <CardContent>
        <CardDescription>
          Codex is drafting the v3 migration. Last activity 2 minutes ago — writing
          the rollback path for the keybindings table.
        </CardDescription>
      </CardContent>
      <CardFooter>
        <span style={{ fontFamily: "var(--mono)" }}>tb/settings-schema · turn 14</span>
      </CardFooter>
    </Card>
  );
}

export function CardGrid() {
  return (
    <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12, maxWidth: "40rem" }}>
      <Card style={{ border: "1px solid var(--border)" }}>
        <CardHeader>
          <CardTitle style={{ fontSize: "1rem" }}>Terminal theme tokens</CardTitle>
        </CardHeader>
        <CardContent>
          <CardDescription>Waiting on your approval to edit 3 files.</CardDescription>
        </CardContent>
        <CardFooter>
          <div style={{ marginRight: "auto" }}>
            <Badge variant="warning" size="sm">
              Needs you
            </Badge>
          </div>
        </CardFooter>
      </Card>
      <Card style={{ border: "1px solid var(--border)" }}>
        <CardHeader>
          <CardTitle style={{ fontSize: "1rem" }}>Stabilize Codex sessions</CardTitle>
        </CardHeader>
        <CardContent>
          <CardDescription>Merged this morning — ETXTBSY retries landed.</CardDescription>
        </CardContent>
        <CardFooter>
          <div style={{ marginRight: "auto" }}>
            <Badge variant="secondary" size="sm">
              Merged
            </Badge>
          </div>
        </CardFooter>
      </Card>
    </div>
  );
}

/**
 * The same card composed inside the dark palette: a `dark`-classed wrapper
 * flips every token, proving the shipped theme works without any extra setup.
 */
export function DarkMode() {
  return (
    <div
      className="dark"
      style={{
        background: "var(--page-background)",
        padding: 20,
        borderRadius: 12,
        maxWidth: "30rem",
      }}
    >
      <Card style={{ border: "1px solid var(--border)" }}>
        <CardHeader>
          <CardTitle>Terminal theme tokens</CardTitle>
          <Badge variant="warning" size="sm">
            Needs you
          </Badge>
        </CardHeader>
        <CardContent>
          <CardDescription>
            Waiting on your approval to edit{" "}
            <span style={{ fontFamily: "var(--mono)" }}>TerminalPane.tsx</span> — the xterm palette
            moves onto the app tokens.
          </CardDescription>
        </CardContent>
        <CardFooter>
          <div style={{ display: "flex", alignItems: "center", gap: 12, width: "100%" }}>
            <span>+9 −3</span>
            <div style={{ marginLeft: "auto", display: "flex", gap: 8 }}>
              <Button size="sm" variant="outline">
                Review diff
              </Button>
              <Button size="sm">Approve</Button>
            </div>
          </div>
        </CardFooter>
      </Card>
    </div>
  );
}
