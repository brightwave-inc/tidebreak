//! An in-process reference implementation of the sandbox-agent boundary.
//!
//! This is the protocol's first consumer and the target the conformance suite
//! runs against in CI. It implements the [`SandboxBackend`] decomposition and a
//! connectable, resumable sandbox that speaks the real wire types — provisioning
//! and addressing, the version handshake, the resumable event stream, reverse
//! RPC over a run-scoped [`CapabilityHost`], and artifact collection.
//!
//! It is not a transport: frames are passed as typed values across in-process
//! channels rather than serialized over a socket, because the contract this
//! backend exists to pin is the protocol's *semantics* (version refusal,
//! deny-by-default, the cursor contract, reverse-RPC correlation / cancellation
//! / disconnect-reissue), not byte framing. Byte framing is a concrete backend's
//! job (delivery-sequence step 7) and is exercised separately; the wire types
//! here all round-trip through serde, pinned by unit tests.
//!
//! The reference sandbox is driven from the test's side through a
//! [`SandboxControl`] — it emits events, exposes artifacts, and originates
//! reverse requests on command. A future container backend stands in for this
//! by running a scripted agent behind the same protocol seam.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::{watch, Semaphore};
use uuid::Uuid;

use crate::{
    artifacts::{ArtifactContent, ArtifactEntry, ArtifactManifest},
    events::{EventBatch, EventPayload, SandboxEvent},
    host::CapabilityHost,
    ids::{EventCursor, OperationId, RunId, Sequence},
    protocol::{
        handshake, AttachAccepted, AttachRefused, AttachRequest, ErrorCode, ErrorResponse,
        HandshakeResponse, Response, MAX_ARTIFACTS, MAX_BUFFERED_EVENTS, MAX_INFLIGHT_REQUESTS,
        PROTOCOL_VERSION,
    },
    provisioning::{
        BackendError, ProvisionRequest, SandboxAddress, SandboxBackend, SandboxHandle,
        TransportSecret,
    },
    reverse::{ReverseEnvelope, ReverseRequest, ReverseResult},
    RequestId,
};

/// Why attaching a host to a sandbox failed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ConnectError {
    /// The versions did not match exactly; the sandbox answered with an on-wire
    /// [`AttachRefused`] carrying its own version, and the connection is not
    /// established.
    #[error("attach refused: sandbox speaks protocol version {}", .0.protocol_version)]
    VersionRefused(AttachRefused),
    /// The presented transport secret did not match; the sandbox answered with an
    /// on-wire [`AttachRefused`], and no session was established or served. The
    /// error text carries no secret.
    #[error("attach refused: the sandbox rejected the transport secret")]
    Unauthenticated(AttachRefused),
    /// The address resolves to no reachable sandbox.
    #[error("sandbox address is not reachable: {0}")]
    Unreachable(String),
}

/// The sandbox could not accept an emitted event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EmitError {
    /// The un-acknowledged event buffer is full; the sandbox has checkpointed
    /// and stopped producing rather than dropping events. A drain that advances
    /// the host's cursor clears this.
    #[error("event buffer overflowed; sandbox checkpointed")]
    Overflow,
    /// The event payload exceeds its declared per-event bound and is refused.
    #[error("event payload exceeds its per-event bound")]
    TooLarge,
}

/// The outcome of one sandbox-originated reverse call over the current session.
#[derive(Debug, Clone)]
pub enum ReverseCallOutcome {
    /// The host settled the call (success or a transport-stable error).
    Settled(Response<ReverseResult>),
    /// The connection dropped before a response arrived; the host's execution
    /// keeps running and records, so re-issuing the same `OperationId` on a
    /// fresh session returns the recorded outcome.
    Disconnected,
}

struct AttachedHost {
    host: CapabilityHost,
    disconnect: watch::Receiver<bool>,
    /// The request lane's in-flight bound for this connection. Reverse requests
    /// acquire a permit and back up when the host is slow; the reserved control
    /// lane (cancel) does not touch it.
    request_permits: Arc<Semaphore>,
}

struct Inner {
    protocol_version: u32,
    buffer_cap: usize,
    request_lane_capacity: usize,
    events: Vec<SandboxEvent>,
    next_seq: u64,
    acked_through: u64,
    overflowed: bool,
    artifacts: HashMap<String, Vec<u8>>,
    attached: Option<AttachedHost>,
}

/// One run-scoped reference sandbox. Cloneable; every clone shares one state.
#[derive(Clone)]
pub struct ReferenceSandbox {
    inner: Arc<Mutex<Inner>>,
}

impl ReferenceSandbox {
    /// A sandbox speaking the current [`PROTOCOL_VERSION`] with the default
    /// event-buffer and request-lane bounds.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(PROTOCOL_VERSION, MAX_BUFFERED_EVENTS, MAX_INFLIGHT_REQUESTS)
    }

    /// A sandbox with explicit protocol version, event-buffer bound, and
    /// request-lane capacity, for exercising version refusal, overflow, and
    /// backpressure at a small, cheap scale.
    #[must_use]
    pub fn with_config(
        protocol_version: u32,
        buffer_cap: usize,
        request_lane_capacity: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                protocol_version,
                buffer_cap: buffer_cap.max(1),
                request_lane_capacity: request_lane_capacity.max(1),
                events: Vec::new(),
                next_seq: Sequence::FIRST.get(),
                acked_through: EventCursor::START.get(),
                overflowed: false,
                artifacts: HashMap::new(),
                attached: None,
            })),
        }
    }

    /// The test-facing handle that drives this sandbox's side of the run.
    #[must_use]
    pub fn control(&self) -> SandboxControl {
        SandboxControl {
            sandbox: self.clone(),
        }
    }

    fn protocol_version(&self) -> u32 {
        self.inner.lock().expect("sandbox lock").protocol_version
    }

    fn latest_sequence(&self) -> Option<Sequence> {
        let inner = self.inner.lock().expect("sandbox lock");
        (inner.next_seq > Sequence::FIRST.get()).then(|| Sequence::new(inner.next_seq - 1))
    }

    fn request_lane_capacity(&self) -> usize {
        self.inner
            .lock()
            .expect("sandbox lock")
            .request_lane_capacity
    }

    fn attach(&self, host: AttachedHost) {
        self.inner.lock().expect("sandbox lock").attached = Some(host);
    }

    fn detach(&self) {
        self.inner.lock().expect("sandbox lock").attached = None;
    }
}

impl Default for ReferenceSandbox {
    fn default() -> Self {
        Self::new()
    }
}

/// The test-facing driver for the sandbox side of a reference run.
#[derive(Clone)]
pub struct SandboxControl {
    sandbox: ReferenceSandbox,
}

impl SandboxControl {
    /// Emit a bounded progress line, returning its assigned sequence.
    ///
    /// # Errors
    /// [`EmitError::Overflow`] when the un-acknowledged buffer is full.
    pub fn emit_progress(&self, text: impl Into<String>) -> Result<Sequence, EmitError> {
        self.emit(EventPayload::Progress(text.into()))
    }

    /// Emit the run's terminal result submission, returning its sequence.
    ///
    /// # Errors
    /// [`EmitError::Overflow`] when the un-acknowledged buffer is full.
    pub fn emit_result(&self, text: impl Into<String>) -> Result<Sequence, EmitError> {
        self.emit(EventPayload::Result(text.into()))
    }

    fn emit(&self, payload: EventPayload) -> Result<Sequence, EmitError> {
        if !payload.within_bounds() {
            return Err(EmitError::TooLarge);
        }
        let mut inner = self.sandbox.inner.lock().expect("sandbox lock");
        let unacked = (inner.next_seq - 1).saturating_sub(inner.acked_through);
        if unacked >= inner.buffer_cap as u64 {
            inner.overflowed = true;
            return Err(EmitError::Overflow);
        }
        let sequence = Sequence::new(inner.next_seq);
        inner.events.push(SandboxEvent { sequence, payload });
        inner.next_seq += 1;
        Ok(sequence)
    }

    /// Expose an artifact for later collection.
    pub fn put_artifact(&self, name: impl Into<String>, bytes: impl Into<Vec<u8>>) {
        self.sandbox
            .inner
            .lock()
            .expect("sandbox lock")
            .artifacts
            .insert(name.into(), bytes.into());
    }

    /// Originate one reverse request over the currently attached session.
    ///
    /// Reverse-RPC availability is keyed to attachment: with no attached host
    /// this returns [`ReverseCallOutcome::Disconnected`]. The call first acquires
    /// a request-lane permit — the backpressure point — so a saturated request
    /// lane blocks new requests here rather than buffering without bound; the
    /// permit is held until the call settles. A disconnect mid-flight fails it
    /// while the host's execution keeps running.
    pub async fn issue_reverse(
        &self,
        operation_id: OperationId,
        request: ReverseRequest,
    ) -> ReverseCallOutcome {
        let Some((host, mut disconnect, permits)) = self.attached() else {
            return ReverseCallOutcome::Disconnected;
        };
        // Backpressure: block on a request-lane permit before registering the
        // request. Held by `_permit` for the lifetime of the call.
        let _permit = match permits.acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return ReverseCallOutcome::Disconnected,
        };
        if *disconnect.borrow() {
            return ReverseCallOutcome::Disconnected;
        }
        let envelope = ReverseEnvelope {
            protocol_version: self.sandbox.protocol_version(),
            request_id: RequestId::new(),
            operation_id,
            request,
        };
        let waiter = host.dispatch(envelope);
        tokio::select! {
            response = waiter.wait() => ReverseCallOutcome::Settled(response),
            () = wait_disconnected(&mut disconnect) => ReverseCallOutcome::Disconnected,
        }
    }

    /// Cancel an in-flight reverse operation over the reserved control lane.
    ///
    /// This reaches the host directly rather than through the request lane, and
    /// acquires no request-lane permit, so it is never queued behind request
    /// backpressure — a cancel lands even while the request lane is saturated.
    pub fn cancel_reverse(&self, operation_id: OperationId) {
        if let Some((host, _, _)) = self.attached() {
            host.cancel(operation_id);
        }
    }

    #[allow(clippy::type_complexity)]
    fn attached(&self) -> Option<(CapabilityHost, watch::Receiver<bool>, Arc<Semaphore>)> {
        let inner = self.sandbox.inner.lock().expect("sandbox lock");
        inner.attached.as_ref().map(|attached| {
            (
                attached.host.clone(),
                attached.disconnect.clone(),
                Arc::clone(&attached.request_permits),
            )
        })
    }
}

async fn wait_disconnected(disconnect: &mut watch::Receiver<bool>) {
    loop {
        if *disconnect.borrow() {
            return;
        }
        // A closed channel (the Session was dropped without setting the flag)
        // is also a disconnect.
        if disconnect.changed().await.is_err() {
            return;
        }
    }
}

/// The host's connection to a reference sandbox. Dropping it models the
/// connection dropping: the sandbox detaches and any in-flight reverse call
/// fails with [`ReverseCallOutcome::Disconnected`].
pub struct Session {
    sandbox: ReferenceSandbox,
    accepted: AttachAccepted,
    disconnect: watch::Sender<bool>,
}

impl Session {
    /// The handshake result: the sandbox's version, the granted capabilities,
    /// and the highest sequence it holds.
    #[must_use]
    pub fn accepted(&self) -> &AttachAccepted {
        &self.accepted
    }

    /// Drain events strictly newer than `cursor`, advancing the sandbox's
    /// acknowledgement in the same call — the host commits a batch and advances
    /// its cursor in one transaction, and a re-delivered sequence at or below
    /// the cursor is never returned.
    #[must_use]
    pub fn events_since(&self, cursor: EventCursor) -> EventBatch {
        let mut inner = self.sandbox.inner.lock().expect("sandbox lock");
        let events: Vec<SandboxEvent> = inner
            .events
            .iter()
            .filter(|event| cursor.precedes(event.sequence))
            .cloned()
            .collect();
        let highest = events
            .last()
            .map_or(cursor.get(), |event| event.sequence.get());
        inner.acked_through = inner.acked_through.max(cursor.get()).max(highest);
        let unacked = (inner.next_seq - 1).saturating_sub(inner.acked_through);
        if unacked < inner.buffer_cap as u64 {
            inner.overflowed = false;
        }
        EventBatch {
            events,
            overflowed: inner.overflowed,
        }
    }

    /// The bounded manifest of artifacts the run exposes.
    ///
    /// # Errors
    /// [`ErrorCode::TooLarge`] if the run exposes more than [`MAX_ARTIFACTS`] —
    /// the manifest is untrusted input and its cardinality is bounded before it
    /// crosses to the host.
    pub fn collect_artifacts(&self) -> Result<ArtifactManifest, ErrorResponse> {
        use sha2::{Digest, Sha256};
        let inner = self.sandbox.inner.lock().expect("sandbox lock");
        if inner.artifacts.len() > MAX_ARTIFACTS {
            return Err(ErrorResponse::new(
                ErrorCode::TooLarge,
                "artifact manifest exceeds its bound",
                false,
            ));
        }
        let mut entries: Vec<ArtifactEntry> = inner
            .artifacts
            .iter()
            .map(|(name, bytes)| ArtifactEntry {
                name: name.clone(),
                bytes: bytes.len(),
                sha256: Sha256::digest(bytes).into(),
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(ArtifactManifest { entries })
    }

    /// Fetch one artifact's bounded bytes.
    ///
    /// # Errors
    /// [`ErrorCode::NotFound`] if no artifact has that name, or
    /// [`ErrorCode::TooLarge`] if it exceeds its bound.
    pub fn fetch_artifact(&self, name: &str) -> Result<ArtifactContent, ErrorResponse> {
        let bytes = {
            let inner = self.sandbox.inner.lock().expect("sandbox lock");
            inner.artifacts.get(name).cloned()
        };
        match bytes {
            Some(bytes) => ArtifactContent::encode(&bytes),
            None => Err(ErrorResponse::new(
                ErrorCode::NotFound,
                "no artifact matches this name",
                false,
            )),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.disconnect.send(true);
        self.sandbox.detach();
    }
}

enum Provisioning {
    Managed,
    SelfHosted,
}

/// An in-process [`SandboxBackend`] plus the attach path the conformance suite
/// drives.
///
/// Two modes exercise the provision/address/destroy decomposition without a
/// special case at attach: `managed` stands up a fresh sandbox per run;
/// `self_hosted` wraps a user-supplied endpoint whose provision and destroy are
/// no-ops. Both resolve to the same [`Session`] through [`ReferenceBackend::connect`].
pub struct ReferenceBackend {
    mode: Provisioning,
    secret: TransportSecret,
    registry: Mutex<HashMap<String, ReferenceSandbox>>,
    handles: Mutex<HashMap<String, String>>,
}

impl ReferenceBackend {
    /// A backend that provisions a fresh sandbox per run.
    #[must_use]
    pub fn managed(secret: TransportSecret) -> Self {
        Self {
            mode: Provisioning::Managed,
            secret,
            registry: Mutex::new(HashMap::new()),
            handles: Mutex::new(HashMap::new()),
        }
    }

    /// A backend wrapping a pre-existing, user-supplied endpoint. Provision and
    /// destroy are no-ops; the sandbox already exists at `base_url`.
    #[must_use]
    pub fn self_hosted(
        base_url: impl Into<String>,
        secret: TransportSecret,
        sandbox: ReferenceSandbox,
    ) -> Self {
        let base_url = base_url.into();
        let registry = Mutex::new(HashMap::from([(base_url, sandbox)]));
        Self {
            mode: Provisioning::SelfHosted,
            secret,
            registry,
            handles: Mutex::new(HashMap::new()),
        }
    }

    /// The driver for the sandbox behind `handle`, if it is registered.
    #[must_use]
    pub fn control(&self, handle: &SandboxHandle) -> Option<SandboxControl> {
        let base_url = self.base_url_for(handle)?;
        self.registry
            .lock()
            .expect("registry lock")
            .get(&base_url)
            .map(ReferenceSandbox::control)
    }

    /// Attach a run-scoped host to the addressed sandbox, performing the version
    /// handshake and wiring reverse RPC to `host`.
    ///
    /// The handshake is computed by the shared [`handshake`] function — the
    /// canonical answer a backend returns — so a skew yields an on-wire
    /// [`AttachRefused`] rather than an out-of-band decision.
    ///
    /// # Errors
    /// [`ConnectError::VersionRefused`] on a version mismatch or
    /// [`ConnectError::Unauthenticated`] on a rejected transport secret (each
    /// carrying the sandbox's on-wire refusal; the connection is not established),
    /// or [`ConnectError::Unreachable`] if the address resolves to no sandbox.
    pub fn connect(
        &self,
        address: &SandboxAddress,
        attach: AttachRequest,
        host: CapabilityHost,
    ) -> Result<Session, ConnectError> {
        let sandbox = self
            .registry
            .lock()
            .expect("registry lock")
            .get(&address.base_url)
            .cloned()
            .ok_or_else(|| ConnectError::Unreachable(address.base_url.clone()))?;

        let accepted = match handshake(
            &attach,
            sandbox.protocol_version(),
            Some(&self.secret),
            host.granted_capabilities(),
            sandbox.latest_sequence(),
        ) {
            HandshakeResponse::Accepted(accepted) => accepted,
            HandshakeResponse::Refused(refused) => {
                return Err(match refused.code {
                    ErrorCode::Unauthenticated => ConnectError::Unauthenticated(refused),
                    _ => ConnectError::VersionRefused(refused),
                });
            }
        };

        let (disconnect_tx, disconnect_rx) = watch::channel(false);
        sandbox.attach(AttachedHost {
            host,
            disconnect: disconnect_rx,
            request_permits: Arc::new(Semaphore::new(sandbox.request_lane_capacity())),
        });
        Ok(Session {
            sandbox,
            accepted,
            disconnect: disconnect_tx,
        })
    }

    fn base_url_for(&self, handle: &SandboxHandle) -> Option<String> {
        self.handles
            .lock()
            .expect("handles lock")
            .get(&handle.reference)
            .cloned()
    }
}

#[async_trait::async_trait]
impl SandboxBackend for ReferenceBackend {
    async fn provision(&self, request: ProvisionRequest) -> Result<SandboxHandle, BackendError> {
        match self.mode {
            Provisioning::Managed => {
                let base_url = format!("inproc://{}", Uuid::new_v4());
                self.registry
                    .lock()
                    .expect("registry lock")
                    .insert(base_url.clone(), ReferenceSandbox::new());
                let reference = provision_reference(request.run_id);
                self.handles
                    .lock()
                    .expect("handles lock")
                    .insert(reference.clone(), base_url);
                Ok(SandboxHandle {
                    reference,
                    tag: request.tag,
                })
            }
            Provisioning::SelfHosted => {
                // No-op: the endpoint already exists. Map a handle onto it so
                // address/destroy travel the same path as the managed case.
                let base_url = self
                    .registry
                    .lock()
                    .expect("registry lock")
                    .keys()
                    .next()
                    .cloned()
                    .ok_or_else(|| {
                        BackendError::Provision("no self-hosted endpoint registered".to_owned())
                    })?;
                let reference = provision_reference(request.run_id);
                self.handles
                    .lock()
                    .expect("handles lock")
                    .insert(reference.clone(), base_url);
                Ok(SandboxHandle {
                    reference,
                    tag: request.tag,
                })
            }
        }
    }

    async fn address(&self, handle: &SandboxHandle) -> Result<SandboxAddress, BackendError> {
        let base_url = self
            .base_url_for(handle)
            .ok_or(BackendError::UnknownHandle)?;
        if !self
            .registry
            .lock()
            .expect("registry lock")
            .contains_key(&base_url)
        {
            return Err(BackendError::Unaddressable(base_url));
        }
        Ok(SandboxAddress {
            base_url,
            transport_secret: self.secret.clone(),
        })
    }

    async fn destroy(&self, handle: &SandboxHandle) -> Result<(), BackendError> {
        let base_url = self.base_url_for(handle);
        self.handles
            .lock()
            .expect("handles lock")
            .remove(&handle.reference);
        match self.mode {
            // Idempotent teardown of the managed sandbox.
            Provisioning::Managed => {
                if let Some(base_url) = base_url {
                    self.registry
                        .lock()
                        .expect("registry lock")
                        .remove(&base_url);
                }
                Ok(())
            }
            // No-op: the user tears down their own endpoint.
            Provisioning::SelfHosted => Ok(()),
        }
    }
}

fn provision_reference(run_id: RunId) -> String {
    format!("{run_id}:{}", Uuid::new_v4())
}
