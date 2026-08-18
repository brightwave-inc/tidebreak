import { HarnessPicker } from "tidebreak-desktop-ui";

const caps = {
  resume: "supported",
  streaming_deltas: "supported",
  structured_approvals: "supported",
  mid_turn_steering: "unsupported",
  plan_mode: "supported",
  auto_mode: "supported",
  allow_mode: "supported",
  reasoning_levels: "unknown",
  native_file_change_events: "unsupported",
  native_interrupt: "supported",
};

function entry(overrides: Record<string, unknown>) {
  return {
    found: true,
    version: "2.1.34",
    path: "/usr/local/bin/harness",
    tier: "reference",
    caps: { ...caps },
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
    ...overrides,
  };
}

export function ReadySelection() {
  return (
    <div style={{ width: "18rem" }}>
      <HarnessPicker
        harnesses={[
          entry({ kind: "claude_code" }),
          entry({ kind: "codex" }),
          entry({ kind: "opencode" }),
        ]}
        value="claude_code"
        onChange={() => {}}
      />
    </div>
  );
}

export function MixedAvailability() {
  return (
    <div style={{ width: "18rem" }}>
      <HarnessPicker
        harnesses={[
          entry({ kind: "claude_code" }),
          entry({ kind: "codex", authenticated: false }),
          entry({ kind: "opencode", found: false }),
        ]}
        value="claude_code"
        onChange={() => {}}
      />
    </div>
  );
}

export function NoneDetected() {
  return (
    <div style={{ width: "18rem" }}>
      <HarnessPicker harnesses={[]} value={null} onChange={() => {}} />
    </div>
  );
}
