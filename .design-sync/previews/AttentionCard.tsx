import { AttentionCard, Button } from "tidebreak-desktop-ui";

export function ClarifyingQuestion() {
  return (
    <AttentionCard
      title="Which crate should own the migration?"
      titleId="attention-question"
      subtitle="The turn-state tables are read by both the server and the desktop sync worker. The agent needs a decision before it edits the schema."
    >
      <div style={{ display: "flex", flexDirection: "column", gap: "0.5rem" }}>
        <Button variant="outline" className="justify-start">
          tidebreak-server — keep migrations beside the queries
        </Button>
        <Button variant="outline" className="justify-start">
          tidebreak-store — a shared home both crates depend on
        </Button>
      </div>
    </AttentionCard>
  );
}

export function BusyDecision() {
  return (
    <AttentionCard
      title="Allow access to ~/code/tidebreak?"
      titleId="attention-busy"
      subtitle="The session wants to read and edit files under this folder for the rest of the turn."
      busy
    >
      <div style={{ display: "flex", gap: "0.5rem" }}>
        <Button disabled>Allowing…</Button>
        <Button variant="outline" disabled>
          Deny
        </Button>
      </div>
    </AttentionCard>
  );
}

export function DecisionError() {
  return (
    <AttentionCard
      title="Approve running `cargo publish`?"
      titleId="attention-error"
      subtitle="This publishes tidebreak-core 0.9.2 to crates.io. Publishing cannot be undone."
      error="The session ended before the decision was applied. Start a new turn and ask again."
    >
      <div style={{ display: "flex", gap: "0.5rem" }}>
        <Button>Approve</Button>
        <Button variant="outline">Deny</Button>
      </div>
    </AttentionCard>
  );
}
