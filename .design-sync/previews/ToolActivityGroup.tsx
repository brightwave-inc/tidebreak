import { ToolActivityGroup, ToolCommandCard } from "tidebreak-desktop-ui";

export function SettledPhase() {
  return (
    <ToolActivityGroup
      groupIndex={0}
      animate={false}
      activities={[
        {
          id: "call_01",
          name: "search",
          status: "completed",
          preview: {
            tool: "search",
            query: "flatten-on-switch replay",
            summary: "Searching the project docs for replay rules",
          },
        },
        { id: "call_02", name: "read_file", status: "completed" },
        { id: "call_03", name: "exec", status: "completed" },
      ]}
    />
  );
}

export function ActivePhase() {
  return (
    <ToolActivityGroup
      groupIndex={1}
      animate={false}
      activities={[
        { id: "call_04", name: "list_dir", status: "completed" },
        {
          id: "call_05",
          name: "exec",
          status: "running",
          preview: {
            tool: "exec",
            command: "cargo",
            args: ["test", "-p", "tidebreak-server"],
            cwd: ".",
            files: [],
          },
        },
      ]}
    >
      <ToolCommandCard
        name="exec"
        status="running"
        preview={{
          tool: "exec",
          command: "cargo",
          args: ["test", "-p", "tidebreak-server"],
          cwd: ".",
          files: [],
        }}
        result={{
          tool: "exec",
          exitCode: null,
          timedOut: false,
          outputTruncated: false,
          stdout:
            "running 214 tests\n" +
            "test journal::replay_flattens_on_provider_switch ... ok\n" +
            "test turn::cancellation_accounts_usage ... ok\n",
          stderr: "",
        }}
      />
    </ToolActivityGroup>
  );
}

export function DelegationPhase() {
  return (
    <ToolActivityGroup
      groupIndex={2}
      animate={false}
      activities={[
        { id: "call_06", name: "spawn_sandbox_agent", status: "completed" },
        { id: "call_07", name: "spawn_sandbox_agent", status: "completed" },
        { id: "call_08", name: "wait_for_agents", status: "completed" },
      ]}
    />
  );
}

export function PhaseWithCommandCard() {
  return (
    <ToolActivityGroup
      groupIndex={3}
      animate={false}
      activities={[
        {
          id: "call_09",
          name: "search",
          status: "completed",
          preview: { tool: "search", query: "approval grant ladder" },
        },
        { id: "call_10", name: "exec", status: "completed" },
      ]}
    >
      <ToolCommandCard
        name="exec"
        status="completed"
        preview={{
          tool: "exec",
          command: "cargo",
          args: ["fmt", "--check"],
          cwd: ".",
          files: [],
          summary: "Check workspace formatting",
        }}
        result={{
          tool: "exec",
          exitCode: 0,
          timedOut: false,
          outputTruncated: false,
          stdout: "",
          stderr: "",
        }}
      />
    </ToolActivityGroup>
  );
}
