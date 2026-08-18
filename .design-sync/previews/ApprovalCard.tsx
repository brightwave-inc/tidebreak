import { ApprovalCard } from "tidebreak-desktop-ui";

const noop = () => {};

export function ExecApproval() {
  return (
    <ApprovalCard
      callId="call_01"
      summary="Allow Tidebreak to run a command that leaves the chat workspace and may reach the network?"
      preview={{
        tool: "exec",
        command: "pnpm",
        args: ["install"],
        cwd: "crates/tidebreak-desktop/ui",
        files: [],
      }}
      canApprove
      canRemember
      grantRungs={[
        "exact_action",
        { command_prefix: { tokens: 1 } },
        { command_prefix: { tokens: 2 } },
        "whole_tool",
      ]}
      deciding={false}
      onDecide={noop}
    />
  );
}

export function WriteFileApproval() {
  return (
    <ApprovalCard
      callId="call_02"
      summary="Allow Tidebreak to create or modify files in this chat's workspace?"
      preview={{
        tool: "write_file",
        path: "crates/tidebreak-server/src/turn.rs",
      }}
      canApprove
      canRemember={false}
      deciding={false}
      onDecide={noop}
    />
  );
}

export function AutoJudgingWithError() {
  return (
    <ApprovalCard
      callId="call_03"
      summary="Allow Tidebreak to run a command that leaves the chat workspace and may reach the network?"
      preview={{
        tool: "exec",
        command: "gh",
        args: ["pr", "checks", "2182"],
        cwd: ".",
        files: [],
      }}
      canApprove
      canRemember
      grantRungs={["exact_action", "whole_tool"]}
      autoJudging
      deciding={false}
      error="The decision could not be recorded — the chat session reconnected. Choose again."
      onDecide={noop}
    />
  );
}
