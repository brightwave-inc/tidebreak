import { ToolCommandCard } from "tidebreak-desktop-ui";

export function Completed() {
  return (
    <ToolCommandCard
      name="exec"
      status="completed"
      preview={{
        tool: "exec",
        command: "cargo",
        args: ["test", "-p", "tidebreak-server"],
        cwd: ".",
        files: [],
        summary: "Run the server test suite",
      }}
      result={{
        tool: "exec",
        exitCode: 0,
        timedOut: false,
        outputTruncated: false,
        stdout:
          "running 214 tests\n" +
          "test journal::replay_flattens_on_provider_switch ... ok\n" +
          "test turn::cancellation_accounts_usage ... ok\n" +
          "\ntest result: ok. 214 passed; 0 failed; 2 ignored; finished in 41.32s\n",
        stderr: "",
      }}
    />
  );
}

export function RunningWithOutput() {
  return (
    <ToolCommandCard
      name="exec"
      status="running"
      preview={{
        tool: "exec",
        command: "cargo",
        args: ["test", "-p", "tidebreak-server", "--", "--nocapture"],
        cwd: ".",
        files: [],
        summary: "Run the server test suite",
      }}
      result={{
        tool: "exec",
        exitCode: null,
        timedOut: false,
        outputTruncated: false,
        stdout:
          "running 214 tests\n" +
          "test journal::replay_flattens_on_provider_switch ... ok\n" +
          "test turn::cancellation_accounts_usage ... ok\n" +
          "test approvals::grant_ladder_orders_narrowest_first ... ok\n",
        stderr: "",
      }}
    />
  );
}

export function RunningWaitingForOutput() {
  return (
    <ToolCommandCard
      name="exec"
      status="running"
      preview={{
        tool: "exec",
        command: "rg",
        args: ["-n", "attention", "crates/"],
        cwd: ".",
        files: [],
      }}
      result={null}
    />
  );
}

export function FailedExit() {
  return (
    <ToolCommandCard
      name="exec"
      status="failed"
      preview={{
        tool: "exec",
        command: "cargo",
        args: ["clippy", "--all-targets", "--", "-D", "warnings"],
        cwd: ".",
        files: [],
        summary: "Lint the workspace with clippy",
      }}
      result={{
        tool: "exec",
        exitCode: 101,
        timedOut: false,
        outputTruncated: false,
        stdout: "",
        stderr:
          "error: unused variable: `turn_id`\n" +
          "  --> crates/tidebreak-server/src/turn.rs:412:9\n" +
          "error: could not compile `tidebreak-server` (lib) due to 1 previous error\n",
        backend: "docker",
      }}
    />
  );
}

export function TimedOutAndDegraded() {
  return (
    <ToolCommandCard
      name="exec"
      status="failed"
      preview={{
        tool: "exec",
        command: "pnpm",
        args: ["build"],
        cwd: "crates/tidebreak-desktop/ui",
        files: [],
        summary: "Build the desktop UI bundle",
      }}
      result={{
        tool: "exec",
        exitCode: null,
        timedOut: true,
        outputTruncated: true,
        stdout:
          "vite v6.0.3 building for production...\ntransforming (412) src/MessageList.tsx",
        stderr: "",
        degraded: "sandbox_image_unavailable",
      }}
    />
  );
}
