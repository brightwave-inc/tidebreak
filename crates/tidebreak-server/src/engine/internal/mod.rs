//! Tidebreak's own agent loop behind the adapter contract.
//!
//! The engine drives [`tidebreak_core::agent::Agent`] through the chat turn
//! lane that already runs it — the durable `turn_run` queue, the
//! [`crate::turn_worker::TurnWorker`], and the chat event journal — and
//! translates what that lane journals into [`tidebreak_harness::HarnessEvent`]s
//! for the session worker's sink. Its durable state is one engine-private
//! conversation per session, keyed by the session id, which owner-scoped
//! chat reads never list. Chat's continuations become durable parks: a
//! user-questions card or a plan proposal ends the leg as
//! [`tidebreak_harness::TurnOutcome::Parked`], and the answer or plan
//! decision resumes it through [`tidebreak_harness::HarnessSession::resume_turn`].

mod adapter;
mod session;
mod translate;

pub(crate) use adapter::InternalAdapter;
