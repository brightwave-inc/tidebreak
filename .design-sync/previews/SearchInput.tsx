import { SearchInput } from "tidebreak-desktop-ui";

const noop = () => {};

export function Sizes() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, maxWidth: 360 }}>
      <SearchInput
        placeholder="Search repos and workspaces"
        aria-label="Search repos and workspaces"
        value=""
        onValueChange={noop}
      />
      <SearchInput
        size="sm"
        placeholder="Filter chats"
        aria-label="Filter chats"
        value=""
        onValueChange={noop}
      />
    </div>
  );
}

export function WithQuery() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, maxWidth: 360 }}>
      <SearchInput
        placeholder="Search repos and workspaces"
        aria-label="Search repos and workspaces"
        value="tb/fix-retry-test"
        onValueChange={noop}
      />
      <SearchInput
        size="sm"
        placeholder="Filter chats"
        aria-label="Filter chats"
        value="settings schema"
        onValueChange={noop}
      />
    </div>
  );
}

export function InPanelHeader() {
  return (
    <div
      className="rounded-md border border-border bg-card"
      style={{ display: "flex", flexDirection: "column", maxWidth: 320 }}
    >
      <div style={{ padding: 8 }}>
        <SearchInput
          size="sm"
          placeholder="Filter chats"
          aria-label="Filter chats"
          value="retry"
          onValueChange={noop}
        />
      </div>
      <div
        className="text-sm"
        style={{ display: "flex", flexDirection: "column", gap: 2, padding: "0 8px 8px" }}
      >
        <span className="rounded-sm bg-accent px-2 py-1 text-accent-foreground">
          Fix flaky retry test
        </span>
        <span className="px-2 py-1">Retry budget for tool calls</span>
        <span className="px-2 py-1 text-muted-foreground">Retry on 429 from Anthropic</span>
      </div>
    </div>
  );
}
