import {
  Badge,
  Button,
  PanelBreadcrumb,
  PanelPrimaryHeader,
  PanelSecondaryHeader,
} from "tidebreak-desktop-ui";

const noop = () => {};

function PanelShell({ children }: { children: React.ReactNode }) {
  return (
    <div
      style={{
        width: 560,
        border: "1px solid var(--border)",
        borderRadius: 10,
        overflow: "hidden",
        background: "var(--background)",
      }}
    >
      {children}
    </div>
  );
}

export function WithBreadcrumb() {
  return (
    <PanelShell>
      <PanelPrimaryHeader
        showBorder
        breadcrumb={
          <PanelBreadcrumb firstPart="Changes" currentItem="crates/tidebreak-server/src/turn.rs" />
        }
        onToggleFullscreen={noop}
        onClose={noop}
      />
      <div style={{ padding: 12, fontSize: 12, color: "var(--muted-foreground)" }}>
        48 additions, 12 deletions
      </div>
    </PanelShell>
  );
}

export function WithActions() {
  return (
    <PanelShell>
      <PanelPrimaryHeader
        showBorder
        breadcrumb={<PanelBreadcrumb firstPart="Pull request" currentItem="#2182" />}
        rightSlot={
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <Badge variant="success" size="sm">
              checks passed
            </Badge>
            <Button size="xs" variant="outline">
              Open on GitHub
            </Button>
            <Button size="xs">Merge</Button>
          </div>
        }
        onToggleFullscreen={noop}
        onClose={noop}
      />
      <div style={{ padding: 12, fontSize: 13 }}>
        feat(desktop): ship ARM64 packages and cross-platform updates
      </div>
    </PanelShell>
  );
}

export function SpaceBetween() {
  return (
    <PanelShell>
      <PanelPrimaryHeader
        spaceBetween
        showBorder
        rightSlot={
          <Button size="xs" variant="ghost">
            New terminal
          </Button>
        }
        onToggleFullscreen={noop}
        onClose={noop}
      />
      <PanelSecondaryHeader className="px-3">
        <span style={{ fontSize: 13, fontWeight: 500 }}>Terminals</span>
        <Badge variant="info" size="sm">
          2 running
        </Badge>
      </PanelSecondaryHeader>
      <div
        style={{
          padding: 12,
          fontFamily: "var(--mono)",
          fontSize: 12,
          color: "var(--muted-foreground)",
        }}
      >
        $ cargo test -p tidebreak-server --locked
      </div>
    </PanelShell>
  );
}

export function Fullscreen() {
  return (
    <PanelShell>
      <PanelPrimaryHeader
        showBorder
        isFullscreen
        breadcrumb={<PanelBreadcrumb firstPart="Workspace" currentItem="tb/fix-retry-test" />}
        leftSlot={
          <Badge variant="warning" size="sm">
            needs you
          </Badge>
        }
        onToggleFullscreen={noop}
        onClose={noop}
      />
      <div style={{ padding: 12, fontSize: 12, color: "var(--muted-foreground)" }}>
        Waiting on approval to run `git push`.
      </div>
    </PanelShell>
  );
}

export function HeaderOnly() {
  return (
    <PanelShell>
      <PanelPrimaryHeader
        breadcrumb={<PanelBreadcrumb firstPart="Sources" currentItem="docs/model-providers.md" />}
      />
      <div style={{ padding: 12, fontSize: 12, color: "var(--muted-foreground)" }}>
        No fullscreen or close callbacks: the chrome buttons stay hidden.
      </div>
    </PanelShell>
  );
}
