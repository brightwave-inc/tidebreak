import { DoctorList } from "tidebreak-desktop-ui";

const readyCaps = {
  resume: "supported",
  streaming_deltas: "supported",
  structured_approvals: "supported",
  mid_turn_steering: "supported",
  plan_mode: "supported",
  auto_mode: "supported",
  allow_mode: "supported",
  reasoning_levels: "supported",
  native_file_change_events: "unsupported",
  native_interrupt: "supported",
};

const codexCaps = {
  resume: "supported",
  streaming_deltas: "supported",
  structured_approvals: "supported",
  mid_turn_steering: "unsupported",
  plan_mode: "unsupported",
  auto_mode: "supported",
  allow_mode: "supported",
  reasoning_levels: "supported",
  native_file_change_events: "supported",
  native_interrupt: "unsupported",
};

const unknownCaps = {
  resume: "unknown",
  streaming_deltas: "unknown",
  structured_approvals: "unknown",
  mid_turn_steering: "unknown",
  plan_mode: "unknown",
  auto_mode: "unknown",
  allow_mode: "unknown",
  reasoning_levels: "unknown",
  native_file_change_events: "unknown",
  native_interrupt: "unknown",
};

const fullReport = {
  harnesses: [
    {
      kind: "claude_code",
      found: true,
      path: "/usr/local/bin/claude",
      version: "2.1.34",
      tier: "reference",
      caps: readyCaps,
      authenticated: true,
      remediation: "",
      stderr: "",
      unrecognized_event_count: 0,
    },
    {
      kind: "codex",
      found: true,
      path: "/opt/homebrew/bin/codex",
      version: "0.48.0",
      tier: "secondary",
      caps: codexCaps,
      authenticated: false,
      remediation: "Sign in: run `codex login` in your terminal, then refresh.",
      stderr: "",
      unrecognized_event_count: 3,
    },
    {
      kind: "opencode",
      found: false,
      tier: "tertiary",
      caps: unknownCaps,
      remediation: "Install opencode and make sure it is on your PATH, then refresh.",
      stderr: "",
      unrecognized_event_count: 0,
    },
  ],
};

const readyReport = {
  harnesses: [
    {
      kind: "claude_code",
      found: true,
      path: "/usr/local/bin/claude",
      version: "2.1.34",
      tier: "reference",
      caps: readyCaps,
      authenticated: true,
      remediation: "",
      stderr: "",
      unrecognized_event_count: 0,
    },
  ],
};

export function FullReport() {
  return (
    <div style={{ maxWidth: "34rem" }}>
      <DoctorList report={fullReport} />
    </div>
  );
}

export function Refreshing() {
  return (
    <div style={{ maxWidth: "34rem" }}>
      <DoctorList report={readyReport} onRefresh={() => {}} refreshing />
    </div>
  );
}
