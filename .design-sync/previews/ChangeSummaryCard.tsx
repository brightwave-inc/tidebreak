import { ChangeSummaryCard } from "tidebreak-desktop-ui";

const client = {
  getFileChangePreview: async () => new Blob(),
  undoFileChange: async () => ({ snapshot_id: "", status: "restored" }),
  undoTurnFileChanges: async () => ({ files: [] }),
};

const retryDiff = `@@ -118,9 +118,10 @@ impl RetryWorker {
-        executor.yield_now().await;
-        let timer = clock.register(self.backoff);
+        let timer = clock.register(self.backoff);
+        executor.yield_now().await;
         let elapsed = timer.observed_delay();
+        debug_assert!(elapsed >= self.backoff);`;

const testDiff = `@@ -41,7 +41,7 @@ fn test_retry_backoff() {
-    assert_eq!(timer.scheduled_delay(), Duration::from_millis(250));
+    assert_eq!(timer.observed_delay(), Duration::from_millis(250));`;

export function EditRun() {
  return (
    <div style={{ maxWidth: "38rem" }}>
      <ChangeSummaryCard
        client={client}
        chatId="chat-01J9"
        turnId="turn-14"
        files={[
          {
            snapshot_id: "snap-1",
            folder_name: "tidebreak",
            relative_path: "crates/scheduler/src/retry.rs",
            classification: "applied",
            change: "overwritten",
            rejection_reason: null,
            undo: "available",
            diff: retryDiff,
            binary_preview: null,
          },
          {
            snapshot_id: "snap-2",
            folder_name: "tidebreak",
            relative_path: "crates/scheduler/tests/retry_backoff.rs",
            classification: "applied",
            change: "overwritten",
            rejection_reason: null,
            undo: "available",
            diff: testDiff,
            binary_preview: null,
          },
          {
            snapshot_id: "snap-3",
            folder_name: "tidebreak",
            relative_path: "crates/scheduler/src/retry_flaky_notes.md",
            classification: "applied",
            change: "deleted",
            rejection_reason: null,
            undo: "available",
            diff: null,
            binary_preview: null,
          },
        ]}
      />
    </div>
  );
}

export function WithRejection() {
  return (
    <div style={{ maxWidth: "38rem" }}>
      <ChangeSummaryCard
        client={client}
        chatId="chat-01J9"
        turnId="turn-15"
        files={[
          {
            snapshot_id: "snap-4",
            folder_name: "tidebreak",
            relative_path: "crates/scheduler/src/lib.rs",
            classification: "applied",
            change: "overwritten",
            rejection_reason: null,
            undo: "already_undone",
            diff: testDiff,
            binary_preview: null,
          },
          {
            snapshot_id: "snap-5",
            folder_name: "tidebreak",
            relative_path: "crates/scheduler/Cargo.toml",
            classification: "rejected",
            change: null,
            rejection_reason: "stale",
            undo: "not_available",
            diff: null,
            binary_preview: null,
          },
        ]}
      />
    </div>
  );
}
