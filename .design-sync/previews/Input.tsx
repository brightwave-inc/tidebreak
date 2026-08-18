import { Input } from "tidebreak-desktop-ui";

export function WorkspaceFields() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, maxWidth: 360 }}>
      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">Workspace title</span>
        <Input defaultValue="Fix flaky retry test" />
      </label>
      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">Base ref</span>
        <Input placeholder="main" />
      </label>
    </div>
  );
}

export function Small() {
  return (
    <div style={{ maxWidth: 280 }}>
      <Input size="sm" placeholder="Filter branches" />
    </div>
  );
}

export function Disabled() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, maxWidth: 360 }}>
      <Input disabled defaultValue="tb/fix-retry-test" />
      <Input disabled placeholder="Locked while the agent is running" />
    </div>
  );
}
