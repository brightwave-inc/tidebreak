//! Native-only lifecycle client for the capability-gated host-broker sidecar.

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex as StdMutex,
    time::Duration,
};

use openwave_host_broker::{
    sidecar::{SidecarRequest, SidecarResponse, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES},
    ControlEnvelope, ControlRequest, ControlResult, ErrorCode, OperationEnvelope, OperationResult,
    RequestId, Response, PROTOCOL_VERSION,
};
use tauri::{async_runtime::JoinHandle, AppHandle};
use tauri_plugin_shell::ShellExt;
use thiserror::Error;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot},
    time::{timeout, timeout_at, Instant},
};

const SIDECAR_NAME: &str = "openwave-host-broker";
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const COMMAND_QUEUE_CAPACITY: usize = 32;
pub(crate) const MUTATION_DISPATCH_WINDOW: Duration = Duration::from_secs(5);

const MINIMAL_ENV_KEYS: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "SystemRoot",
    "SystemDrive",
    "USERPROFILE",
    "PATHEXT",
    "windir",
];

/// Serializes the synchronous sidecar protocol behind one lazy child process.
pub(crate) struct BrokerClient {
    commands: mpsc::Sender<BrokerCommand>,
    task: StdMutex<Option<JoinHandle<()>>>,
}

impl BrokerClient {
    pub(crate) fn new(app: AppHandle, data_dir: PathBuf, home_dir: PathBuf) -> Self {
        let (commands, receiver) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
        let task = tauri::async_runtime::spawn(
            BrokerWorker {
                app,
                data_dir,
                home_dir,
                session: None,
            }
            .run(receiver),
        );
        Self {
            commands,
            task: StdMutex::new(Some(task)),
        }
    }

    pub(crate) async fn control(
        &self,
        request: ControlRequest,
    ) -> Result<ControlResult, BrokerClientError> {
        self.send_control(request, true, None).await
    }

    /// Send one control frame without replaying an ambiguous native mutation.
    pub(crate) async fn control_without_retry(
        &self,
        request: ControlRequest,
        dispatch_deadline: Instant,
    ) -> Result<ControlResult, BrokerClientError> {
        self.send_control(request, false, Some(dispatch_deadline))
            .await
    }

    async fn send_control(
        &self,
        request: ControlRequest,
        retry: bool,
        dispatch_deadline: Option<Instant>,
    ) -> Result<ControlResult, BrokerClientError> {
        let (reply, result) = oneshot::channel();
        self.commands
            .try_send(BrokerCommand::Control {
                request,
                retry,
                dispatch_deadline,
                reply,
            })
            .map_err(map_admission_error)?;
        result.await.map_err(|_| BrokerClientError::Closed)?
    }

    pub(crate) async fn operation(
        &self,
        envelope: OperationEnvelope,
    ) -> Result<OperationResult, BrokerClientError> {
        let (reply, result) = oneshot::channel();
        self.commands
            .try_send(BrokerCommand::Operation { envelope, reply })
            .map_err(map_admission_error)?;
        result.await.map_err(|_| BrokerClientError::Closed)?
    }

    pub(crate) async fn shutdown(&self) {
        let (reply, finished) = oneshot::channel();
        let acknowledged = matches!(
            timeout(SHUTDOWN_TIMEOUT + SHUTDOWN_TIMEOUT, async {
                self.commands
                    .send(BrokerCommand::Shutdown { reply })
                    .await
                    .map_err(|_| ())?;
                finished.await.map_err(|_| ())
            })
            .await,
            Ok(Ok(()))
        );
        let task = self
            .task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(task) = task {
            if !acknowledged {
                task.abort();
            }
            let _ = task.await;
        }
    }
}

enum BrokerCommand {
    Control {
        request: ControlRequest,
        retry: bool,
        dispatch_deadline: Option<Instant>,
        reply: oneshot::Sender<Result<ControlResult, BrokerClientError>>,
    },
    Operation {
        envelope: OperationEnvelope,
        reply: oneshot::Sender<Result<OperationResult, BrokerClientError>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

fn map_admission_error(error: mpsc::error::TrySendError<BrokerCommand>) -> BrokerClientError {
    match error {
        mpsc::error::TrySendError::Full(_) => BrokerClientError::Busy,
        mpsc::error::TrySendError::Closed(_) => BrokerClientError::Closed,
    }
}

struct BrokerWorker {
    app: AppHandle,
    data_dir: PathBuf,
    home_dir: PathBuf,
    session: Option<Session>,
}

impl BrokerWorker {
    async fn run(mut self, mut commands: mpsc::Receiver<BrokerCommand>) {
        while let Some(command) = commands.recv().await {
            match command {
                BrokerCommand::Control {
                    request,
                    retry,
                    dispatch_deadline,
                    reply,
                } => {
                    let result = if retry {
                        self.control(request).await
                    } else {
                        self.control_once(request, dispatch_deadline).await
                    };
                    let _ = reply.send(result);
                }
                BrokerCommand::Operation { envelope, reply } => {
                    let result = self
                        .exchange(SidecarRequest::Operation(envelope))
                        .await
                        .and_then(|result| match result {
                            ExchangeResult::Operation(result) => Ok(result),
                            ExchangeResult::Control(_) => Err(BrokerClientError::Protocol),
                        });
                    let _ = reply.send(result);
                }
                BrokerCommand::Shutdown { reply } => {
                    self.stop_session().await;
                    let _ = reply.send(());
                    return;
                }
            }
        }
        self.stop_session().await;
    }

    async fn control(
        &mut self,
        request: ControlRequest,
    ) -> Result<ControlResult, BrokerClientError> {
        let first = self.control_once(request.clone(), None).await;
        if first
            .as_ref()
            .is_err_and(BrokerClientError::retryable_control)
        {
            return self.control_once(request, None).await;
        }
        first
    }

    async fn control_once(
        &mut self,
        request: ControlRequest,
        dispatch_deadline: Option<Instant>,
    ) -> Result<ControlResult, BrokerClientError> {
        let envelope = ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            request,
        };
        match self
            .exchange_before(SidecarRequest::Control(envelope), dispatch_deadline)
            .await?
        {
            ExchangeResult::Control(result) => Ok(result),
            ExchangeResult::Operation(_) => Err(BrokerClientError::Protocol),
        }
    }

    async fn exchange(
        &mut self,
        request: SidecarRequest,
    ) -> Result<ExchangeResult, BrokerClientError> {
        self.exchange_before(request, None).await
    }

    async fn exchange_before(
        &mut self,
        request: SidecarRequest,
        dispatch_deadline: Option<Instant>,
    ) -> Result<ExchangeResult, BrokerClientError> {
        if dispatch_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(BrokerClientError::DispatchExpired);
        }
        if self.session.is_none() {
            self.session = Some(Session::start(&self.app, &self.data_dir, &self.home_dir).await?);
        }
        if dispatch_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(BrokerClientError::DispatchExpired);
        }
        let exchange = self
            .session
            .as_mut()
            .expect("session initialized")
            .exchange(request);
        let result = match dispatch_deadline {
            Some(deadline) => timeout_at(deadline, exchange)
                .await
                .map_err(|_| BrokerClientError::DispatchExpired)
                .and_then(|result| result),
            None => timeout(REQUEST_TIMEOUT, exchange)
                .await
                .map_err(|_| BrokerClientError::Timeout)
                .and_then(|result| result),
        };
        if result
            .as_ref()
            .is_err_and(BrokerClientError::poisons_session)
        {
            self.stop_session().await;
        }
        result
    }

    async fn stop_session(&mut self) {
        if let Some(session) = self.session.take() {
            session.stop().await;
        }
    }
}

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    async fn start(
        app: &AppHandle,
        data_dir: &Path,
        home_dir: &Path,
    ) -> Result<Self, BrokerClientError> {
        let mut sidecar = app
            .shell()
            .sidecar(SIDECAR_NAME)
            .map_err(|_| BrokerClientError::Start)?
            .args([
                OsStr::new("--data-dir"),
                data_dir.as_os_str(),
                OsStr::new("--home"),
                home_dir.as_os_str(),
            ])
            .env_clear();
        sidecar = sidecar.envs(minimal_environment());

        let command: std::process::Command = sidecar.into();
        let mut command = Command::from(command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| BrokerClientError::Start)?;
        let stdin = child.stdin.take().ok_or(BrokerClientError::Start)?;
        let stdout = child.stdout.take().ok_or(BrokerClientError::Start)?;
        let mut session = Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
        };

        let hello_id = RequestId::new();
        let hello = SidecarRequest::Control(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: hello_id,
            request: ControlRequest::Hello,
        });
        let result = timeout(HELLO_TIMEOUT, session.exchange(hello))
            .await
            .map_err(|_| BrokerClientError::Timeout)
            .and_then(|result| result);
        match result {
            Ok(ExchangeResult::Control(ControlResult::Hello(hello)))
                if hello.protocol_version == PROTOCOL_VERSION =>
            {
                Ok(session)
            }
            Ok(_) => {
                session.stop().await;
                Err(BrokerClientError::Protocol)
            }
            Err(error) => {
                session.stop().await;
                Err(error)
            }
        }
    }

    async fn exchange(
        &mut self,
        request: SidecarRequest,
    ) -> Result<ExchangeResult, BrokerClientError> {
        let (expected_channel, expected_id) = match &request {
            SidecarRequest::Control(envelope) => (Channel::Control, envelope.request_id),
            SidecarRequest::Operation(envelope) => (Channel::Operation, envelope.request_id),
        };
        let mut encoded = serde_json::to_vec(&request).map_err(|_| BrokerClientError::Protocol)?;
        if encoded.len() > MAX_REQUEST_BYTES {
            return Err(BrokerClientError::Protocol);
        }
        encoded.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or(BrokerClientError::Closed)?;
        stdin
            .write_all(&encoded)
            .await
            .map_err(|_| BrokerClientError::Closed)?;
        stdin.flush().await.map_err(|_| BrokerClientError::Closed)?;

        let frame = read_frame(&mut self.stdout).await?;
        let response: SidecarResponse =
            serde_json::from_slice(&frame).map_err(|_| BrokerClientError::Protocol)?;
        decode_response(response, expected_channel, expected_id)
    }

    async fn stop(mut self) {
        drop(self.stdin.take());
        if timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await.is_ok() {
            return;
        }
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Channel {
    Control,
    Operation,
}

enum ExchangeResult {
    Control(ControlResult),
    Operation(OperationResult),
}

fn decode_response(
    response: SidecarResponse,
    expected_channel: Channel,
    expected_id: RequestId,
) -> Result<ExchangeResult, BrokerClientError> {
    match response {
        SidecarResponse::Control(envelope) if expected_channel == Channel::Control => {
            validate_envelope(envelope.protocol_version, envelope.request_id, expected_id)?;
            match envelope.response {
                Response::Ok(result) => Ok(ExchangeResult::Control(result)),
                Response::Error(error) => Err(BrokerClientError::Broker {
                    code: error.code,
                    message: error.message,
                    retryable: error.retryable,
                }),
            }
        }
        SidecarResponse::Operation(envelope) if expected_channel == Channel::Operation => {
            validate_envelope(envelope.protocol_version, envelope.request_id, expected_id)?;
            match envelope.response {
                Response::Ok(result) => Ok(ExchangeResult::Operation(result)),
                Response::Error(error) => Err(BrokerClientError::Broker {
                    code: error.code,
                    message: error.message,
                    retryable: error.retryable,
                }),
            }
        }
        SidecarResponse::TransportError(error) => {
            if error
                .request_id
                .is_some_and(|request_id| request_id != expected_id)
            {
                return Err(BrokerClientError::Protocol);
            }
            Err(BrokerClientError::Transport(error.message))
        }
        SidecarResponse::Control(_) | SidecarResponse::Operation(_) => {
            Err(BrokerClientError::Protocol)
        }
    }
}

fn validate_envelope(
    protocol_version: u32,
    request_id: RequestId,
    expected_id: RequestId,
) -> Result<(), BrokerClientError> {
    if protocol_version != PROTOCOL_VERSION || request_id != expected_id {
        return Err(BrokerClientError::Protocol);
    }
    Ok(())
}

async fn read_frame(input: &mut (impl AsyncBufRead + Unpin)) -> Result<Vec<u8>, BrokerClientError> {
    let mut frame = Vec::new();
    loop {
        let available = input
            .fill_buf()
            .await
            .map_err(|_| BrokerClientError::Closed)?;
        if available.is_empty() {
            return Err(BrokerClientError::Closed);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let data_len = newline.unwrap_or(available.len());
        if frame.len().saturating_add(data_len) > MAX_RESPONSE_BYTES {
            return Err(BrokerClientError::ResponseTooLarge);
        }
        frame.extend_from_slice(&available[..data_len]);
        let consumed = newline.map_or(available.len(), |position| position + 1);
        input.consume(consumed);
        if newline.is_some() {
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            if frame.is_empty() {
                return Err(BrokerClientError::Protocol);
            }
            return Ok(frame);
        }
    }
}

fn minimal_environment() -> Vec<(OsString, OsString)> {
    MINIMAL_ENV_KEYS
        .iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(key), value)))
        .collect()
}

#[derive(Debug, Error)]
pub(crate) enum BrokerClientError {
    #[error("host broker could not start")]
    Start,
    #[error("host broker is busy; try again")]
    Busy,
    #[error("host broker mutation could not start before its authority deadline")]
    DispatchExpired,
    #[error("host broker connection closed")]
    Closed,
    #[error("host broker request timed out")]
    Timeout,
    #[error("host broker returned an invalid response")]
    Protocol,
    #[error("host broker response exceeded its size limit")]
    ResponseTooLarge,
    #[error("host broker transport error: {0}")]
    Transport(String),
    #[error("{message}")]
    Broker {
        code: ErrorCode,
        message: String,
        retryable: bool,
    },
}

impl BrokerClientError {
    fn poisons_session(&self) -> bool {
        !matches!(self, Self::Broker { .. })
    }

    fn retryable_control(&self) -> bool {
        matches!(
            self,
            Self::Closed
                | Self::Timeout
                | Self::Broker {
                    retryable: true,
                    ..
                }
        )
    }
}

#[cfg(test)]
mod tests {
    use openwave_host_broker::{
        sidecar::{SidecarResponse, TransportError, TransportErrorCode},
        ControlResponseEnvelope, ControlResult, HelloResult, Response,
    };

    use super::*;

    #[test]
    fn validates_channel_version_and_correlation() {
        let request_id = RequestId::new();
        let ok = SidecarResponse::Control(ControlResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            response: Response::Ok(ControlResult::Hello(HelloResult {
                protocol_version: PROTOCOL_VERSION,
                operations: vec!["list_roots".to_owned()],
            })),
        });
        assert!(matches!(
            decode_response(ok, Channel::Control, request_id),
            Ok(ExchangeResult::Control(ControlResult::Hello(_)))
        ));

        let wrong_channel = SidecarResponse::Control(ControlResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            response: Response::Ok(ControlResult::Hello(HelloResult {
                protocol_version: PROTOCOL_VERSION,
                operations: Vec::new(),
            })),
        });
        assert!(matches!(
            decode_response(wrong_channel, Channel::Operation, request_id),
            Err(BrokerClientError::Protocol)
        ));

        let wrong_id = SidecarResponse::Control(ControlResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            response: Response::Ok(ControlResult::Hello(HelloResult {
                protocol_version: PROTOCOL_VERSION,
                operations: Vec::new(),
            })),
        });
        assert!(matches!(
            decode_response(wrong_id, Channel::Control, request_id),
            Err(BrokerClientError::Protocol)
        ));
    }

    #[test]
    fn uncorrelated_transport_error_applies_only_to_serialized_request() {
        let request_id = RequestId::new();
        let error = SidecarResponse::TransportError(TransportError {
            request_id: None,
            code: TransportErrorCode::MalformedRequest,
            message: "bad frame".to_owned(),
        });
        assert!(matches!(
            decode_response(error, Channel::Control, request_id),
            Err(BrokerClientError::Transport(message)) if message == "bad frame"
        ));
    }

    #[tokio::test]
    async fn response_framing_is_bounded_and_requires_a_nonempty_line() {
        let mut complete = BufReader::new(b"{\"ok\":true}\r\n".as_slice());
        assert_eq!(read_frame(&mut complete).await.unwrap(), br#"{"ok":true}"#);

        let mut empty = BufReader::new(b"\n".as_slice());
        assert!(matches!(
            read_frame(&mut empty).await,
            Err(BrokerClientError::Protocol)
        ));

        let oversized = [vec![b'x'; MAX_RESPONSE_BYTES + 1], b"\n".to_vec()].concat();
        let mut oversized = BufReader::new(oversized.as_slice());
        assert!(matches!(
            read_frame(&mut oversized).await,
            Err(BrokerClientError::ResponseTooLarge)
        ));
    }
}
