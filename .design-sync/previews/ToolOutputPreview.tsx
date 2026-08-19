import { ToolOutputPreview } from "tidebreak-desktop-ui";

const testRun = `$ cargo test -p tidebreak-server --locked
   Compiling tidebreak-core v0.0.0
   Compiling tidebreak-harness v0.0.0
   Compiling tidebreak-server v0.0.0
running 214 tests
test journal::replay_flattens_on_provider_switch ... ok
test journal::records_turn_boundaries ... ok
test turn::cancellation_accounts_usage ... ok
test turn::retries_transient_failure ... ok
test approvals::grant_ladder_orders_narrowest_first ... ok
test approvals::deny_with_feedback_steers ... ok
test recovery::dead_pid_closes_turn_interrupted ... ok
test recovery::live_pid_fences_until_reap ... ok
test attention::stall_sweep_marks_idle_sessions ... ok
test result: ok. 214 passed; 0 failed; 2 ignored; finished in 41.32s`;

export function ClampedWithExpander() {
  return (
    <div style={{ maxWidth: "40rem" }}>
      <ToolOutputPreview text={testRun} label="Command output" />
    </div>
  );
}

export function ShortOutput() {
  return (
    <div style={{ maxWidth: "40rem" }}>
      <ToolOutputPreview text={"18,204"} label="Command output" />
    </div>
  );
}
