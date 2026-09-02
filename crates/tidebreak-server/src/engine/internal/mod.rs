//! Tidebreak's own agent loop behind the adapter contract.
//!
//! The engine drives [`tidebreak_core::agent::Agent`] through the chat turn
//! lane that already runs it — the durable `turn_run` queue and the
//! [`crate::engine::internal::leg::LegDriver`]. The lane journals the turn straight
//! into the session's code journal (decision 0048 step 5), the one journal
//! every engine writes, so the engine has nothing to translate: it admits
//! the turn, follows that journal, and hands the session worker only the
//! approval rows a consent card or a park needs and the terminal outcome
//! that closes the worker's turn row. Its durable state is the session row
//! itself: a session with no workspace is a chat, read by the chat routes
//! under the same id. Chat's continuations become durable parks: a
//! user-questions card or a plan proposal ends the leg as
//! [`tidebreak_harness::TurnOutcome::Parked`], and the answer or plan
//! decision resumes it through [`tidebreak_harness::HarnessSession::resume_turn`].

mod adapter;
pub(crate) mod leg;
mod session;

pub(crate) use adapter::InternalAdapter;
