import {
  AttentionBadge,
  Logomark,
  Sidebar,
  SidebarButton,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarSectionTitle,
} from "tidebreak-desktop-ui";

const chatIcon = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
    <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
  </svg>
);

const codeIcon = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
    <polyline points="16 18 22 12 16 6" />
    <polyline points="8 6 2 12 8 18" />
  </svg>
);

const terminalIcon = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
    <polyline points="4 17 10 11 4 5" />
    <line x1="12" y1="19" x2="20" y2="19" />
  </svg>
);

const pullRequestIcon = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
    <circle cx="6" cy="6" r="3" />
    <circle cx="6" cy="18" r="3" />
    <circle cx="18" cy="18" r="3" />
    <path d="M6 9v6" />
    <path d="M18 15V9a3 3 0 0 0-3-3h-3" />
    <path d="m15 3-3 3 3 3" />
  </svg>
);

const settingsIcon = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
  </svg>
);

const plusIcon = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
    <line x1="12" y1="5" x2="12" y2="19" />
    <line x1="5" y1="12" x2="19" y2="12" />
  </svg>
);

/**
 * The rail sizes itself with `flex-basis`, so every cell puts it in a flex row
 * with a real height — the same shape as the app shell it lives in.
 */
function Shell({ children }: { children?: unknown }) {
  return (
    <div
      className="bg-background text-foreground"
      style={{
        display: "flex",
        height: "26rem",
        border: "1px solid var(--border)",
        borderRadius: "0.5rem",
        overflow: "hidden",
      }}
    >
      {children as any}
    </div>
  );
}

function Canvas({ title, body }: { title: string; body: string }) {
  return (
    <div
      className="bg-muted/30"
      style={{
        flex: "1 1 auto",
        minWidth: 0,
        display: "flex",
        flexDirection: "column",
        gap: "0.5rem",
        padding: "1.25rem",
        borderLeft: "1px solid var(--border)",
      }}
    >
      <span className="text-sm font-medium">{title}</span>
      <span className="text-muted-foreground text-sm">{body}</span>
    </div>
  );
}

export function NavigationRail() {
  return (
    <Shell>
      <Sidebar>
        <SidebarHeader>
          <Logomark width={22} height={12} />
          <span className="text-sm font-medium">Tidebreak</span>
        </SidebarHeader>
        <SidebarContent>
          <SidebarSectionTitle>Workspace</SidebarSectionTitle>
          <div style={{ padding: "0 0.5rem" }}>
            <SidebarButton className="bg-muted">
              {chatIcon}
              <span>Chats</span>
            </SidebarButton>
            <SidebarButton>
              {codeIcon}
              <span>Code</span>
            </SidebarButton>
            <SidebarButton>
              {terminalIcon}
              <span>Terminals</span>
            </SidebarButton>
            <SidebarButton>
              {pullRequestIcon}
              <span>Pull requests</span>
            </SidebarButton>
          </div>
        </SidebarContent>
        <SidebarFooter>
          <SidebarButton>
            {settingsIcon}
            <span>Settings</span>
          </SidebarButton>
        </SidebarFooter>
      </Sidebar>
      <Canvas
        title="tidebreak / main"
        body="Pick a section on the left. The rail keeps its width between launches."
      />
    </Shell>
  );
}

export function SessionList() {
  const sessions: [string, object | null][] = [
    [
      "fix-retry-backoff",
      {
        state: { type: "needs_you", prompt: "Approve `cargo publish`?", source: "structured" },
        source: "structured",
      },
    ],
    ["migrate-settings-schema", { state: { type: "working" }, source: "lifecycle" }],
    ["terminal-theme-tokens", { state: { type: "stalled", idle_secs: 240 }, source: "heuristic" }],
    ["release-notes-draft", { state: { type: "done_unreviewed" }, source: "lifecycle" }],
    ["docs-decision-0037", null],
  ];
  return (
    <Shell>
      <Sidebar>
        <SidebarHeader>
          <span className="text-sm font-medium" style={{ flex: 1, minWidth: 0 }}>
            Code sessions
          </span>
          <SidebarButton style={{ width: "auto", padding: "0.25rem" }} aria-label="New session">
            {plusIcon}
          </SidebarButton>
        </SidebarHeader>
        <SidebarContent>
          <SidebarSectionTitle>Active</SidebarSectionTitle>
          <div style={{ padding: "0 0.5rem" }}>
            {sessions.map(([branch, attention]) => (
              <SidebarButton key={branch}>
                {codeIcon}
                <span className="truncate" style={{ flex: 1, minWidth: 0 }}>
                  {branch}
                </span>
                {attention ? <AttentionBadge attention={attention} compact /> : null}
              </SidebarButton>
            ))}
          </div>
        </SidebarContent>
        <SidebarFooter>
          <SidebarButton>
            {settingsIcon}
            <span>Settings</span>
          </SidebarButton>
        </SidebarFooter>
      </Sidebar>
      <Canvas
        title="fix-retry-backoff"
        body="Needs you — the engine is waiting on approval to publish."
      />
    </Shell>
  );
}

export function SectionedRail() {
  return (
    <Shell>
      <Sidebar>
        <SidebarHeader>
          <Logomark width={22} height={12} />
          <span className="text-sm font-medium">tidebreak</span>
        </SidebarHeader>
        <SidebarContent>
          <SidebarSectionTitle>Chats</SidebarSectionTitle>
          <div style={{ padding: "0 0.5rem" }}>
            <SidebarButton className="bg-muted">
              {chatIcon}
              <span className="truncate">Why does the retry test flake?</span>
            </SidebarButton>
            <SidebarButton>
              {chatIcon}
              <span className="truncate">Release checklist for 0.9</span>
            </SidebarButton>
          </div>
          <SidebarSectionTitle style={{ marginTop: "0.75rem" }}>Terminals</SidebarSectionTitle>
          <div style={{ padding: "0 0.5rem" }}>
            <SidebarButton>
              {terminalIcon}
              <span className="truncate">cargo watch</span>
            </SidebarButton>
            <SidebarButton disabled>
              {terminalIcon}
              <span className="truncate">pnpm dev (exited)</span>
            </SidebarButton>
          </div>
        </SidebarContent>
      </Sidebar>
      <Canvas
        title="Why does the retry test flake?"
        body="Section titles fade out when the rail collapses to icons."
      />
    </Shell>
  );
}
