//! Engines that run inside the server process.
//!
//! An in-process engine implements the same adapter contract
//! (`tidebreak_harness::HarnessAdapter` / `HarnessSession`) as the external
//! CLIs, so the code runtime, its session worker, and every route speak to
//! it exactly as they speak to Claude Code or Codex: sessions, turns, the
//! journal, and approvals. Nothing reaches an engine here another way
//! (decision 0048 step 5).

pub(crate) mod internal;
