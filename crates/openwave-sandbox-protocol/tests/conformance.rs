//! The protocol conformance suite, surfaced as individual CI checks.
//!
//! Each test drives one scenario from [`openwave_sandbox_protocol::conformance`]
//! against the in-process reference backend. Once the local container backend
//! lands (delivery-sequence step 7.1), the same scenarios re-point at it behind
//! the protocol seam; the assertions do not change.

use openwave_sandbox_protocol::conformance;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_mismatch_is_refused() {
    conformance::version_mismatch_is_refused().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deny_by_default_refuses_ungranted_capability() {
    conformance::deny_by_default_refuses_ungranted_capability().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_stream_resumes_from_committed_cursor() {
    conformance::event_stream_resumes_from_committed_cursor().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_buffer_overflow_checkpoints_and_resumes() {
    conformance::event_buffer_overflow_checkpoints_and_resumes().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reverse_rpc_correlates_concurrent_calls() {
    conformance::reverse_rpc_correlates_concurrent_calls().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reverse_rpc_cancel_aborts_in_flight() {
    conformance::reverse_rpc_cancel_aborts_in_flight().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reverse_rpc_disconnect_fails_inflight_then_reissue_replays() {
    conformance::reverse_rpc_disconnect_fails_inflight_then_reissue_replays().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reverse_rpc_reissue_with_a_different_request_conflicts() {
    conformance::reverse_rpc_reissue_with_a_different_request_conflicts().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_hosted_and_managed_share_one_attach_path() {
    conformance::self_hosted_and_managed_share_one_attach_path().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn artifact_collection_roundtrips_and_is_bounded() {
    conformance::artifact_collection_roundtrips_and_is_bounded().await;
}
