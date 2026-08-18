import { ChatStatusChip } from "tidebreak-desktop-ui";

const noop = () => {};

const folders = [
  {
    displayName: "tidebreak",
    statements: [
      { verb: { kind: "capability", capability: "read_files" } },
      { verb: { kind: "capability", capability: "write_files" } },
    ],
  },
];

export function ActivityCard() {
  return (
    <div style={{ maxWidth: "20rem" }}>
      <ChatStatusChip
        outputCount={3}
        folders={folders}
        runs={[
          { id: "run-1", status: "running" },
          { id: "run-2", status: "retry_wait" },
          { id: "run-3", status: "succeeded" },
        ]}
        permissionCount={2}
        onOpenOutputs={noop}
        onOpenFolders={noop}
        onOpenPermissions={noop}
        onOpenAgents={noop}
      />
    </div>
  );
}

export function CompactRunning() {
  return (
    <div style={{ display: "flex", justifyContent: "flex-start" }}>
      <ChatStatusChip
        compact
        outputCount={1}
        folders={folders}
        runs={[
          { id: "run-1", status: "running" },
          { id: "run-2", status: "running" },
          { id: "run-3", status: "waiting" },
          { id: "run-4", status: "queued" },
          { id: "run-5", status: "succeeded" },
        ]}
        permissionCount={2}
        onOpenOutputs={noop}
        onOpenFolders={noop}
        onOpenPermissions={noop}
        onOpenAgents={noop}
      />
    </div>
  );
}

export function CompactQuiet() {
  return (
    <div style={{ display: "flex", justifyContent: "flex-start" }}>
      <ChatStatusChip
        compact
        outputCount={0}
        folders={[]}
        runs={[]}
        onOpenOutputs={noop}
        onOpenFolders={noop}
        onOpenPermissions={noop}
        onOpenAgents={noop}
      />
    </div>
  );
}
