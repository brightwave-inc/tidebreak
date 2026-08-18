import { AttentionBadge, SidebarButton } from "tidebreak-desktop-ui";

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

const settingsIcon = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
  </svg>
);

function Rail({ children }: { children?: unknown }) {
  return (
    <div
      className="bg-background"
      style={{
        width: "15rem",
        display: "flex",
        flexDirection: "column",
        gap: "0.125rem",
        padding: "0.5rem",
        borderRadius: "0.5rem",
        border: "1px solid var(--border, #e5e5e5)",
      }}
    >
      {children as any}
    </div>
  );
}

export function Rows() {
  return (
    <Rail>
      <SidebarButton>
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
    </Rail>
  );
}

export function ActiveRow() {
  return (
    <Rail>
      <SidebarButton>
        {chatIcon}
        <span>Chats</span>
      </SidebarButton>
      <SidebarButton className="bg-muted">
        {codeIcon}
        <span>Code</span>
      </SidebarButton>
      <SidebarButton>
        {settingsIcon}
        <span>Settings</span>
      </SidebarButton>
    </Rail>
  );
}

export function SessionRowWithStatus() {
  return (
    <Rail>
      <SidebarButton>
        {codeIcon}
        <span style={{ flex: 1, minWidth: 0 }} className="truncate">
          fix-retry-backoff
        </span>
        <AttentionBadge
          attention={{
            state: { type: "needs_you", prompt: "Needs you", source: "structured" },
            source: "structured",
          }}
          compact
        />
      </SidebarButton>
      <SidebarButton>
        {codeIcon}
        <span style={{ flex: 1, minWidth: 0 }} className="truncate">
          release-notes-draft
        </span>
        <AttentionBadge
          attention={{ state: { type: "done_unreviewed" }, source: "lifecycle" }}
          compact
        />
      </SidebarButton>
    </Rail>
  );
}

export function DisabledRow() {
  return (
    <Rail>
      <SidebarButton disabled>
        {terminalIcon}
        <span>Terminals (offline)</span>
      </SidebarButton>
    </Rail>
  );
}
