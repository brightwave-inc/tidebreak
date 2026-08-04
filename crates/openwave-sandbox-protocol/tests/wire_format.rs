//! Wire-format spec for the sandbox-agent protocol.
//!
//! Two things every wire type must satisfy, and a symmetric round-trip alone
//! proves neither:
//!
//! - **Round-trip**: `serialize` then `deserialize` yields the original value.
//! - **Golden encoding**: the exact JSON shape — discriminant keys and field
//!   names — matches this spec, so a silent serde `tag`/`rename_all` change is
//!   caught and a cross-language self-hoster has a representation to implement
//!   against, exactly as `openwave-host-broker` pins `encoded["request"]["control"]`.
//!
//! Byte framing and lane multiplexing are out of scope here (they are a concrete
//! backend's, per the crate docs); this file pins the *types* that cross the
//! boundary.

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

use openwave_sandbox_protocol::{
    artifacts::{ArtifactContent, ArtifactEntry, ArtifactManifest},
    events::{EventBatch, EventPayload, SandboxEvent},
    ids::{EventCursor, OperationId, RequestId, RunId, SandboxTag, Sequence},
    AttachAccepted, AttachRefused, AttachRequest, Capability, ControlFrame, ErrorCode,
    ErrorResponse, HandshakeResponse, ModelInferenceParams, ModelInferenceResult, ProvisionRequest,
    RequestFrame, Response, ReverseEnvelope, ReverseRequest, ReverseResponseEnvelope,
    ReverseResult, RunProvenance, SandboxAddress, SandboxHandle, TransportSecret, PROTOCOL_VERSION,
};

/// Assert a value round-trips through JSON unchanged.
fn roundtrip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_string(value).expect("serialize");
    let decoded: T = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(&decoded, value, "round-trip changed the value");
}

/// Assert a value's encoded JSON has exactly the given top-level shape at the
/// named pointers.
fn golden<T: Serialize>(value: &T, checks: &[(&str, Value)]) {
    let encoded = serde_json::to_value(value).expect("serialize");
    for (pointer, expected) in checks {
        let found = encoded
            .pointer(pointer)
            .unwrap_or_else(|| panic!("missing pointer {pointer} in {encoded}"));
        assert_eq!(found, expected, "golden mismatch at {pointer}");
    }
}

#[test]
fn attach_handshake_wire_shapes() {
    let request = AttachRequest {
        protocol_version: PROTOCOL_VERSION,
        run_id: RunId::new(),
        resume_from: EventCursor::committed(Sequence::new(3)),
        transport_secret: TransportSecret::new("per-run-secret"),
    };
    roundtrip(&request);
    golden(
        &request,
        &[
            ("/protocol_version", json!(PROTOCOL_VERSION)),
            ("/resume_from", json!(3)),
            // The secret is transparent on the wire (a bearer token), so a
            // self-hoster's host presents it as a plain string field.
            ("/transport_secret", json!("per-run-secret")),
        ],
    );

    // An attach that omits the secret decodes to the empty token (serde default),
    // which the sandbox refuses — a naive peer gets a clean auth refusal.
    let omitted: AttachRequest = serde_json::from_value(
        json!({"protocol_version": PROTOCOL_VERSION, "run_id": RunId::new(), "resume_from": 0}),
    )
    .expect("an attach without a secret still decodes");
    assert_eq!(omitted.transport_secret, TransportSecret::default());

    let accepted = HandshakeResponse::Accepted(AttachAccepted {
        protocol_version: PROTOCOL_VERSION,
        granted_capabilities: vec![Capability::ModelInference],
        latest_sequence: Some(Sequence::new(3)),
    });
    roundtrip(&accepted);
    golden(
        &accepted,
        &[
            ("/accepted/protocol_version", json!(PROTOCOL_VERSION)),
            ("/accepted/granted_capabilities/0", json!("model_inference")),
            ("/accepted/latest_sequence", json!(3)),
        ],
    );

    let refused = HandshakeResponse::Refused(AttachRefused {
        protocol_version: PROTOCOL_VERSION + 1,
        code: ErrorCode::ProtocolVersion,
    });
    roundtrip(&refused);
    golden(
        &refused,
        &[
            ("/refused/protocol_version", json!(PROTOCOL_VERSION + 1)),
            ("/refused/code", json!("protocol_version")),
        ],
    );
}

#[test]
fn error_and_response_wire_shapes() {
    let error = ErrorResponse::new(ErrorCode::TooLarge, "too big", false);
    roundtrip(&error);
    golden(
        &error,
        &[("/code", json!("too_large")), ("/retryable", json!(false))],
    );

    let ok: Response<u32> = Response::Ok(7);
    golden(&ok, &[("/status", json!("ok")), ("/payload", json!(7))]);
    roundtrip(&ok);

    let err: Response<u32> = Response::Error(ErrorResponse::denied());
    golden(
        &err,
        &[
            ("/status", json!("error")),
            ("/payload/code", json!("denied")),
        ],
    );
    roundtrip(&err);
}

#[test]
fn reverse_request_and_result_wire_shapes() {
    golden(
        &Capability::ModelInference,
        &[("", json!("model_inference"))],
    );

    let request = ReverseRequest::ModelInference(ModelInferenceParams {
        prompt: "hi".to_owned(),
    });
    roundtrip(&request);
    golden(
        &request,
        &[
            ("/capability", json!("model_inference")),
            ("/payload/prompt", json!("hi")),
        ],
    );

    let result = ReverseResult::ModelInference(ModelInferenceResult {
        completion: "done".to_owned(),
    });
    roundtrip(&result);
    golden(
        &result,
        &[
            ("/capability", json!("model_inference")),
            ("/payload/completion", json!("done")),
        ],
    );
}

#[test]
fn reverse_envelopes_and_frames_wire_shapes() {
    // `RequestId`/`OperationId` are `Copy` and serialize transparently as their
    // UUID string, so capturing them lets us pin the enclosing field name
    // exactly rather than merely round-tripping it.
    let request_id = RequestId::new();
    let operation_id = OperationId::new();
    let envelope = ReverseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        operation_id,
        request: ReverseRequest::ModelInference(ModelInferenceParams {
            prompt: "q".to_owned(),
        }),
    };
    roundtrip(&envelope);
    golden(
        &envelope,
        &[
            ("/protocol_version", json!(PROTOCOL_VERSION)),
            ("/request_id", json!(request_id.to_string())),
            ("/operation_id", json!(operation_id.to_string())),
            ("/request/capability", json!("model_inference")),
            ("/request/payload/prompt", json!("q")),
        ],
    );

    let response = ReverseResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        operation_id,
        response: Response::Ok(ReverseResult::ModelInference(ModelInferenceResult {
            completion: "a".to_owned(),
        })),
    };
    roundtrip(&response);
    golden(
        &response,
        &[
            ("/protocol_version", json!(PROTOCOL_VERSION)),
            ("/request_id", json!(request_id.to_string())),
            ("/operation_id", json!(operation_id.to_string())),
            ("/response/status", json!("ok")),
            ("/response/payload/capability", json!("model_inference")),
            ("/response/payload/payload/completion", json!("a")),
        ],
    );

    let request_frame = RequestFrame::Request(envelope);
    roundtrip(&request_frame);
    golden(&request_frame, &[("/frame", json!("request"))]);

    let response_frame = RequestFrame::Response(response);
    golden(&response_frame, &[("/frame", json!("response"))]);
    roundtrip(&response_frame);
}

#[test]
fn control_lane_frames_wire_shapes() {
    let cancel = ControlFrame::Cancel {
        operation_id: OperationId::new(),
    };
    roundtrip(&cancel);
    golden(&cancel, &[("/control", json!("cancel"))]);

    let operation_id = OperationId::new();
    let acknowledge = ControlFrame::Acknowledge { operation_id };
    roundtrip(&acknowledge);
    golden(
        &acknowledge,
        &[
            ("/control", json!("acknowledge")),
            ("/body/operation_id", json!(operation_id.to_string())),
        ],
    );

    let ping = ControlFrame::Ping { nonce: 9 };
    roundtrip(&ping);
    golden(
        &ping,
        &[("/control", json!("ping")), ("/body/nonce", json!(9))],
    );

    let pong = ControlFrame::Pong { nonce: 9 };
    roundtrip(&pong);
    golden(&pong, &[("/control", json!("pong"))]);

    let keepalive = ControlFrame::Keepalive;
    roundtrip(&keepalive);
    golden(&keepalive, &[("/control", json!("keepalive"))]);
}

#[test]
fn event_stream_wire_shapes() {
    let progress = SandboxEvent {
        sequence: Sequence::new(1),
        payload: EventPayload::Progress("working".to_owned()),
    };
    roundtrip(&progress);
    golden(
        &progress,
        &[
            ("/sequence", json!(1)),
            ("/payload/kind", json!("progress")),
            ("/payload/body", json!("working")),
        ],
    );

    let result = SandboxEvent {
        sequence: Sequence::new(2),
        payload: EventPayload::Result("final".to_owned()),
    };
    roundtrip(&result);
    golden(&result, &[("/payload/kind", json!("result"))]);

    // The other terminal event: the loop ended without a result.
    let failed = SandboxEvent {
        sequence: Sequence::new(3),
        payload: EventPayload::Failed("step budget exhausted".to_owned()),
    };
    roundtrip(&failed);
    golden(
        &failed,
        &[
            ("/payload/kind", json!("failed")),
            ("/payload/body", json!("step budget exhausted")),
        ],
    );
    assert!(failed.payload.is_terminal() && result.payload.is_terminal());
    assert!(!progress.payload.is_terminal());

    let batch = EventBatch {
        events: vec![progress],
        overflowed: false,
    };
    roundtrip(&batch);
    golden(
        &batch,
        &[
            ("/overflowed", json!(false)),
            ("/events/0/sequence", json!(1)),
        ],
    );
}

#[test]
fn artifact_wire_shapes() {
    let entry = ArtifactEntry {
        name: "report.md".to_owned(),
        bytes: 5,
        sha256: [0u8; 32],
    };
    roundtrip(&entry);
    golden(
        &entry,
        &[("/name", json!("report.md")), ("/bytes", json!(5))],
    );

    let manifest = ArtifactManifest {
        entries: vec![entry],
    };
    roundtrip(&manifest);
    golden(&manifest, &[("/entries/0/name", json!("report.md"))]);

    let content = ArtifactContent::encode(b"hello").expect("encode");
    roundtrip(&content);
    golden(&content, &[("/bytes", json!(5))]);
    // The digest is a 32-byte array on the wire.
    let encoded = serde_json::to_value(&content).unwrap();
    assert_eq!(encoded["sha256"].as_array().map(Vec::len), Some(32));
}

#[test]
fn provisioning_wire_shapes() {
    let request = ProvisionRequest {
        run_id: RunId::new(),
        tag: SandboxTag::new(),
        lifetime_cap_secs: Some(3600),
        network_policy: openwave_sandbox_protocol::SandboxNetworkPolicy {
            allow_all_public: false,
            allowed_hosts: vec!["api.example.com".to_owned()],
            https_only_hosts: vec!["pypi.org".to_owned()],
        },
    };
    roundtrip(&request);
    golden(
        &request,
        &[
            ("/lifetime_cap_secs", json!(3600)),
            ("/network_policy/allowed_hosts/0", json!("api.example.com")),
            ("/network_policy/https_only_hosts/0", json!("pypi.org")),
        ],
    );

    // A request that predates the policy field deserializes to deny-all, so an
    // older caller provisions a no-egress sandbox rather than an open one.
    let legacy: ProvisionRequest = serde_json::from_value(json!({
        "run_id": RunId::new(),
        "tag": SandboxTag::new(),
        "lifetime_cap_secs": null,
    }))
    .unwrap();
    assert!(legacy.network_policy.denies_everything());

    let handle = SandboxHandle {
        reference: "container-abc".to_owned(),
        tag: SandboxTag::new(),
    };
    roundtrip(&handle);
    golden(&handle, &[("/reference", json!("container-abc"))]);

    let address = SandboxAddress {
        base_url: "https://sandbox.example".to_owned(),
        transport_secret: TransportSecret::new("s3cr3t"),
    };
    roundtrip(&address);
    golden(
        &address,
        &[
            ("/base_url", json!("https://sandbox.example")),
            // The secret is transparent on the wire (a bearer credential), even
            // though it is redacted in Debug output.
            ("/transport_secret", json!("s3cr3t")),
        ],
    );
}

#[test]
fn run_provenance_wire_shape() {
    let provenance = RunProvenance {
        run_id: RunId::new(),
        provider: "reference".to_owned(),
    };
    roundtrip(&provenance);
    golden(&provenance, &[("/provider", json!("reference"))]);
}
