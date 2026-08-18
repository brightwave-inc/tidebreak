import { ScrollableContainer } from "tidebreak-desktop-ui";

export function CommandOutput() {
  return (
    <div style={{ width: 440 }}>
      <ScrollableContainer className="rounded-md border bg-muted/30 p-3 font-mono text-xs">
        {`$ cargo test -p tidebreak-server --locked
   Compiling tidebreak-server v0.1.0
    Finished test profile in 42.18s
     Running unittests src/lib.rs

running 18 tests
test journal::records_turn_boundaries ... ok
test journal::replays_in_order ... ok
test turn::retries_transient_failure ... ok
test turn::caps_retry_budget ... ok
test turn::cancellation_accounts_usage ... ok
test session::resumes_from_checkpoint ... ok
test session::flattens_on_provider_switch ... ok
test session::rejects_stale_schema ... ok
test approvals::blocks_until_decision ... ok
test approvals::expires_after_timeout ... ok
test registry::default_model_is_curated ... ok
test registry::no_image_input_before_path ... ok
test workspace::isolates_worktrees ... ok
test workspace::cleans_unchanged ... ok
test checks::reports_clippy_warnings ... ok
test checks::gates_on_lockfile_drift ... ok
test release::retries_draft_lookup ... ok
test release::signs_updater_feed ... ok

test result: ok. 18 passed; 0 failed; 0 ignored`}
      </ScrollableContainer>
    </div>
  );
}

export function ShortOutput() {
  return (
    <div style={{ width: 440 }}>
      <ScrollableContainer className="rounded-md border bg-muted/30 p-3 font-mono text-xs">
        {`$ git push origin tb/fix-retry-test
To github.com:tidebreak/tidebreak.git
   846bf6b..3d39f55  tb/fix-retry-test -> tb/fix-retry-test`}
      </ScrollableContainer>
    </div>
  );
}

export function DiffPreview() {
  return (
    <div style={{ width: 440 }}>
      <ScrollableContainer className="rounded-md border bg-muted/30 p-3 font-mono text-xs">
        {`--- a/crates/tidebreak-server/src/turn.rs
+++ b/crates/tidebreak-server/src/turn.rs
@@ -118,9 +118,14 @@ impl TurnRunner {
-    fn retry(&mut self) -> Result<()> {
-        self.attempts += 1;
-        self.run()
+    fn retry(&mut self) -> Result<()> {
+        if self.attempts >= self.budget {
+            return Err(Error::RetryBudgetExhausted);
+        }
+        self.attempts += 1;
+        self.journal.record_attempt(self.attempts);
+        self.run()
     }

@@ -204,6 +209,8 @@ impl TurnRunner {
     fn record_outcome(&mut self, outcome: Outcome) {
+        self.journal.record_usage(&outcome.usage);
         self.journal.record_outcome(&outcome);
     }`}
      </ScrollableContainer>
    </div>
  );
}
