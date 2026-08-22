//! Server-side orchestration for an external agent-engine session.
//!
//! Persistence lives in `tidebreak-core::db::code`. Protocol translation lives
//! in `tidebreak-harness`. This module owns git worktrees, the per-session
//! worker, crash recovery, and the live event bus.

pub(crate) mod approval_bridge;
pub(crate) mod approval_sweep;
pub(crate) mod attention;
pub(crate) mod browser_channel;
pub(crate) mod browser_runtime;
pub(crate) mod bus;
pub(crate) mod checkpoint;
pub(crate) mod clone;
pub(crate) mod delivery;
pub(crate) mod fork;
pub(crate) mod gh;
pub(crate) mod harness_install;
pub(crate) mod pr_facts;
pub(crate) mod reconcile;
pub(crate) mod recovery;
pub(crate) mod runtime;
pub(crate) mod scoped;
pub(crate) mod scratch;
pub(crate) mod session_worker;
pub(crate) mod setup_script;
pub(crate) mod terminal;
pub(crate) mod titling;
pub(crate) mod trigger;
pub(crate) mod watch;
pub(crate) mod worktree;
pub(crate) mod worktree_orphans;
pub(crate) mod worktree_root;

pub(crate) use runtime::CodeRuntime;
pub(crate) use scoped::ScopedCode;
