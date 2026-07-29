//! The sandbox side of the reverse channel: the supervisor's RPC client.
//!
//! The supervisor multiplexes many reverse requests over one connection.
//! Correlation is by `RequestId`: a reader task fans each response frame to the
//! waiting caller's one-shot. Two bounds cap how much a slow host lets the
//! sandbox buffer — a semaphore of in-flight permits and a bounded outbound
//! frame queue — so `call` awaits rather than growing memory when the host
//! stops draining. A dropped connection fails every in-flight call with
//! [`ClientError::Disconnected`], and each is safe to re-issue against the same
//! host by the idempotency rule, carrying the same `OperationId`.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader},
    sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore},
};

use crate::{
    protocol::{
        CancelFrame, ErrorResponse, Frame, OperationId, RequestId, Response, ReverseEnvelope,
        ReverseRequest, ReverseResult, PROTOCOL_VERSION,
    },
    transport::{read_frame, write_frame},
};

/// Why one reverse call did not return a result.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The connection dropped; re-issue the same operation identity to retry.
    #[error("the reverse-rpc connection dropped before a response arrived")]
    Disconnected,
    /// The host answered with a transport-stable error (denied, cancelled,
    /// conflict, version, internal).
    #[error("the host rejected the reverse request: {}", .0.message)]
    Rejected(ErrorResponse),
}

/// What the reader task delivers to a waiting caller.
enum CallOutcome {
    Settled(Response<ReverseResult>),
    Disconnected,
}

struct ClientShared {
    outbound: mpsc::Sender<Frame>,
    pending: Mutex<HashMap<RequestId, oneshot::Sender<CallOutcome>>>,
    permits: Arc<Semaphore>,
    closed: AtomicBool,
}

impl ClientShared {
    /// Fail every in-flight call once, on the first disconnect to be observed.
    fn fail_all(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut pending = self.pending.lock().expect("pending lock");
        for (_, sender) in pending.drain() {
            let _ = sender.send(CallOutcome::Disconnected);
        }
    }
}

/// The supervisor's reverse-RPC client over one connection.
#[derive(Clone)]
pub struct ReverseClient {
    shared: Arc<ClientShared>,
}

impl ReverseClient {
    /// Open a client over `stream`, allowing `max_in_flight` outstanding calls.
    ///
    /// `max_in_flight` bounds both the in-flight semaphore and the outbound
    /// frame queue, so neither the request path nor the write path can buffer
    /// without limit while the host is slow.
    #[must_use]
    pub fn connect<S>(stream: S, max_in_flight: usize) -> Self
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let capacity = max_in_flight.max(1);
        let (read_half, write_half) = tokio::io::split(stream);
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Frame>(capacity);
        let shared = Arc::new(ClientShared {
            outbound: outbound_tx,
            pending: Mutex::new(HashMap::new()),
            permits: Arc::new(Semaphore::new(capacity)),
            closed: AtomicBool::new(false),
        });

        // Writer task: serialize the outbound queue onto the wire.
        let writer_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let mut write_half = write_half;
            while let Some(frame) = outbound_rx.recv().await {
                if write_frame(&mut write_half, &frame).await.is_err() {
                    break;
                }
            }
            writer_shared.fail_all();
        });

        // Reader task: fan responses back to their waiting callers.
        let reader_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            loop {
                match read_frame(&mut reader).await {
                    Ok(Frame::Response(envelope)) => {
                        let sender = reader_shared
                            .pending
                            .lock()
                            .expect("pending lock")
                            .remove(&envelope.request_id);
                        if let Some(sender) = sender {
                            let _ = sender.send(CallOutcome::Settled(envelope.response));
                        }
                    }
                    // The supervisor issues requests and cancels; it does not
                    // service them. Ignore rather than trusting peer input.
                    Ok(Frame::Request(_) | Frame::Cancel(_)) => {}
                    Err(_) => break,
                }
            }
            reader_shared.fail_all();
        });

        Self { shared }
    }

    /// Issue one reverse request under the durable `operation_id`.
    ///
    /// Awaits an in-flight permit first — this is the backpressure point — then
    /// registers correlation state and enqueues the request frame. The returned
    /// [`Call`] resolves to the host's answer and can be cancelled meanwhile.
    pub async fn call(
        &self,
        operation_id: OperationId,
        request: ReverseRequest,
    ) -> Result<Call, ClientError> {
        let permit = Arc::clone(&self.shared.permits)
            .acquire_owned()
            .await
            .map_err(|_| ClientError::Disconnected)?;
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(ClientError::Disconnected);
        }

        let request_id = RequestId::new();
        let (tx, rx) = oneshot::channel();
        self.shared
            .pending
            .lock()
            .expect("pending lock")
            .insert(request_id, tx);

        let envelope = ReverseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            operation_id,
            request,
        };
        if self
            .shared
            .outbound
            .send(Frame::Request(envelope))
            .await
            .is_err()
        {
            self.shared
                .pending
                .lock()
                .expect("pending lock")
                .remove(&request_id);
            return Err(ClientError::Disconnected);
        }

        Ok(Call {
            request_id,
            operation_id,
            rx,
            shared: Arc::clone(&self.shared),
            _permit: permit,
        })
    }
}

/// A single in-flight reverse call. Awaiting [`Call::wait`] releases its
/// in-flight permit; [`Call::cancel`] asks the host to abort it.
pub struct Call {
    request_id: RequestId,
    operation_id: OperationId,
    rx: oneshot::Receiver<CallOutcome>,
    shared: Arc<ClientShared>,
    _permit: OwnedSemaphorePermit,
}

impl Call {
    /// Transport correlation identity of this attempt.
    #[must_use]
    pub fn request_id(&self) -> RequestId {
        self.request_id
    }

    /// Durable operation identity carried by this call. Re-issue with the same
    /// value after a disconnect to receive the recorded outcome.
    #[must_use]
    pub fn operation_id(&self) -> OperationId {
        self.operation_id
    }

    /// Ask the host to cancel this in-flight operation. Best-effort: if the
    /// outbound queue is momentarily full the signal is dropped, matching a real
    /// transport where a cancel can lose the race with completion.
    pub fn cancel(&self) {
        let _ = self.shared.outbound.try_send(Frame::Cancel(CancelFrame {
            request_id: self.request_id,
            operation_id: self.operation_id,
        }));
    }

    /// Await the host's answer.
    pub async fn wait(self) -> Result<ReverseResult, ClientError> {
        match self.rx.await {
            Ok(CallOutcome::Settled(Response::Ok(result))) => Ok(result),
            Ok(CallOutcome::Settled(Response::Error(error))) => Err(ClientError::Rejected(error)),
            Ok(CallOutcome::Disconnected) | Err(_) => Err(ClientError::Disconnected),
        }
    }
}
