//! Server-side orchestration for an external agent-engine session.
//!
//! Persistence lives in `tidebreak-core::db::code`. Protocol translation lives
//! in `tidebreak-harness`. This module owns git worktrees, the per-session
//! worker, crash recovery, and the live event bus.

pub(crate) mod approval_bridge;
pub(crate) mod attention;
pub(crate) mod bus;
pub(crate) mod checkpoint;
pub(crate) mod clone;
pub(crate) mod delivery;
pub(crate) mod gh;
pub(crate) mod harness_install;
pub(crate) mod recovery;
pub(crate) mod runtime;
pub(crate) mod scoped;
pub(crate) mod session_worker;
pub(crate) mod setup_script;
pub(crate) mod terminal;
pub(crate) mod titling;
pub(crate) mod watch;
pub(crate) mod worktree;
pub(crate) mod worktree_root;

pub(crate) use runtime::CodeRuntime;
pub(crate) use scoped::ScopedCode;
