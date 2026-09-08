//! Per-session Chrome target tables and event materialization.
//!
//! The dispatch service owns isolation checks and the CDP module owns
//! transport; this file is the designated home for the durable per-session
//! target table when the runtime slice grows past in-memory tables.
