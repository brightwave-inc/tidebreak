//! Native-only lifecycle client for the capability-gated host-broker sidecar.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex as StdMutex,
    time::Duration,
};

#[cfg(target_os = "macos")]
use tauri::Manager;
use tauri::{async_runtime::JoinHandle, AppHandle};
use tauri_plugin_shell::ShellExt;
use thiserror::Error;
use tidebreak_host_broker::{
    sidecar::{SidecarRequest, SidecarResponse, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES},
    ControlEnvelope, ControlRequest, ControlResult, ErrorCode, OperationEnvelope, OperationResult,
    RequestId, Response, PROTOCOL_VERSION,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot},
    time::{timeout, timeout_at, Instant},
};

const SIDECAR_NAME: &str = "tidebreak-host-broker";
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
// Leave room for a 30s native condition wait and the helper's bounded shutdown.
// The helper may cancel, exit, and run its release process before it replies.
const REQUEST_TIMEOUT: Duration =
    Duration::from_secs(tidebreak_host_broker::computer_use::HELPER_MANAGED_TIMEOUT.as_secs() + 5);
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
    admission: StdMutex<BrokerAdmission>,
    task: StdMutex<Option<JoinHandle<()>>>,
    native_cancel_path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrokerAdmission {
    Running,
    Quiescing,
    Quiesced,
    Resuming,
    Shutdown,
}

impl BrokerClient {
    pub(crate) fn new(app: AppHandle, data_dir: PathBuf, home_dir: PathBuf) -> Self {
        let (commands, receiver) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
        let native_cancel_path = native_cancel_path(&data_dir);
        if let Err(error) =
            write_native_generation(&native_cancel_path, &uuid::Uuid::new_v4().to_string())
        {
            eprintln!("tidebreak-desktop: native computer control is unavailable: {error}");
        }
        let task = tauri::async_runtime::spawn(
            BrokerWorker {
                app,
                data_dir,
                home_dir,
                execute_commands: tidebreak_code_execution::LocalExecutionProvider::availability()
                    .is_ok(),
                session: None,
                allow_session_start: true,
            }
            .run(receiver),
        );
        Self {
            commands,
            admission: StdMutex::new(BrokerAdmission::Running),
            task: StdMutex::new(Some(task)),
            native_cancel_path,
        }
    }

    /// Interrupt helper input without waiting behind a running broker call.
    pub(crate) fn cancel_native_actions(&self) -> Result<(), BrokerClientError> {
        write_native_generation(&self.native_cancel_path, "stopped").map_err(|error| {
            // Missing state also cancels input if an atomic replacement fails.
            let _ = std::fs::remove_file(&self.native_cancel_path);
            BrokerClientError::Transport(error.to_string())
        })
    }

    pub(crate) fn resume_native_actions(&self) -> Result<(), BrokerClientError> {
        write_native_generation(&self.native_cancel_path, &uuid::Uuid::new_v4().to_string())
            .map_err(|error| BrokerClientError::Transport(error.to_string()))
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
        self.admit(BrokerCommand::Control {
            request,
            retry,
            dispatch_deadline,
            reply,
        })?;
        result.await.map_err(|_| BrokerClientError::Closed)?
    }

    pub(crate) async fn operation(
        &self,
        envelope: OperationEnvelope,
    ) -> Result<OperationResult, BrokerClientError> {
        let (reply, result) = oneshot::channel();
        self.admit(BrokerCommand::Operation { envelope, reply })?;
        result.await.map_err(|_| BrokerClientError::Closed)?
    }

    fn admit(&self, command: BrokerCommand) -> Result<(), BrokerClientError> {
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *admission != BrokerAdmission::Running {
            return Err(BrokerClientError::Quiesced);
        }
        // Keep the admission lock through enqueue. Once quiesce changes the
        // state, every accepted command is already ordered before its barrier.
        self.commands.try_send(command).map_err(map_admission_error)
    }

    /// Close admission, drain every command accepted before the barrier, and
    /// pin an old-bundle sidecar process across app replacement.
    pub(crate) async fn quiesce_for_update(&self) -> Result<(), BrokerClientError> {
        self.begin_transition(BrokerAdmission::Running, BrokerAdmission::Quiescing)?;
        let (reply, result) = oneshot::channel();
        if self
            .commands
            .send(BrokerCommand::Quiesce { reply })
            .await
            .is_err()
        {
            self.set_admission(BrokerAdmission::Shutdown);
            return Err(BrokerClientError::Closed);
        }
        let result = match result.await {
            Ok(result) => result,
            Err(_) => {
                self.set_admission(BrokerAdmission::Shutdown);
                return Err(BrokerClientError::Closed);
            }
        };
        match result {
            Ok(()) => {
                if self.finish_transition(BrokerAdmission::Quiescing, BrokerAdmission::Quiesced) {
                    Ok(())
                } else {
                    Err(BrokerClientError::Closed)
                }
            }
            Err(error) => {
                self.finish_transition(BrokerAdmission::Quiescing, BrokerAdmission::Running);
                Err(error)
            }
        }
    }

    /// Reopen admission after installation failed, but keep the worker pinned
    /// to the old sidecar process. If that process later dies, host operations
    /// fail safely until relaunch instead of resolving a binary from a bundle
    /// the failed installer may have partially replaced.
    pub(crate) async fn resume_after_failed_update(&self) -> Result<(), BrokerClientError> {
        self.begin_transition(BrokerAdmission::Quiesced, BrokerAdmission::Resuming)?;
        let (reply, result) = oneshot::channel();
        if self
            .commands
            .send(BrokerCommand::ResumeAfterFailedUpdate { reply })
            .await
            .is_err()
        {
            self.set_admission(BrokerAdmission::Shutdown);
            return Err(BrokerClientError::Closed);
        }
        if result.await.is_err() {
            self.set_admission(BrokerAdmission::Shutdown);
            return Err(BrokerClientError::Closed);
        }
        if self.finish_transition(BrokerAdmission::Resuming, BrokerAdmission::Running) {
            Ok(())
        } else {
            Err(BrokerClientError::Closed)
        }
    }

    fn begin_transition(
        &self,
        expected: BrokerAdmission,
        next: BrokerAdmission,
    ) -> Result<(), BrokerClientError> {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *admission != expected {
            return Err(if *admission == BrokerAdmission::Shutdown {
                BrokerClientError::Closed
            } else {
                BrokerClientError::Quiesced
            });
        }
        *admission = next;
        Ok(())
    }

    fn finish_transition(&self, expected: BrokerAdmission, next: BrokerAdmission) -> bool {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *admission == expected {
            *admission = next;
            true
        } else {
            false
        }
    }

    fn set_admission(&self, next: BrokerAdmission) {
        *self
            .admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = next;
    }

    pub(crate) async fn shutdown(&self) {
        let _ = self.cancel_native_actions();
        self.set_admission(BrokerAdmission::Shutdown);
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
    Quiesce {
        reply: oneshot::Sender<Result<(), BrokerClientError>>,
    },
    ResumeAfterFailedUpdate {
        reply: oneshot::Sender<()>,
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
    execute_commands: bool,
    session: Option<Session>,
    allow_session_start: bool,
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
                BrokerCommand::Quiesce { reply } => {
                    // The command itself is the queue barrier. Admission was
                    // closed before it was enqueued, so all earlier work and
                    // any retry it performs have completed. Starting here is
                    // still from the old bundle and gives a failed install a
                    // coherent process to resume without path resolution.
                    let result = self.ensure_session().await.map(|_| ());
                    let _ = reply.send(result);
                }
                BrokerCommand::ResumeAfterFailedUpdate { reply } => {
                    self.allow_session_start = false;
                    let _ = reply.send(());
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
        self.ensure_session().await?;
        if dispatch_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(BrokerClientError::DispatchExpired);
        }
        let result = self
            .session
            .as_mut()
            .expect("session initialized")
            .exchange_before(request, dispatch_deadline)
            .await;
        if result
            .as_ref()
            .is_err_and(BrokerClientError::poisons_session)
        {
            self.stop_session().await;
        }
        result
    }

    async fn ensure_session(&mut self) -> Result<&mut Session, BrokerClientError> {
        if self.session.is_none() {
            if !self.allow_session_start {
                return Err(BrokerClientError::UpdateRecovery);
            }
            self.session = Some(
                Session::start(
                    &self.app,
                    &self.data_dir,
                    &self.home_dir,
                    self.execute_commands,
                )
                .await?,
            );
        }
        Ok(self.session.as_mut().expect("session initialized"))
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
        execute_commands: bool,
    ) -> Result<Self, BrokerClientError> {
        let mut args = vec![
            OsString::from("--data-dir"),
            data_dir.as_os_str().to_owned(),
            OsString::from("--home"),
            home_dir.as_os_str().to_owned(),
        ];
        if execute_commands {
            args.push(OsString::from("--execute-commands"));
        }
        let mut sidecar = app
            .shell()
            .sidecar(SIDECAR_NAME)
            .map_err(|_| BrokerClientError::Start)?
            .args(args)
            .env_clear();
        sidecar = sidecar.envs(minimal_environment());
        sidecar = sidecar.env(
            tidebreak_host_broker::HELPER_CANCEL_PATH_ENV,
            native_cancel_path(data_dir),
        );
        #[cfg(target_os = "macos")]
        if let Some(helper) = computer_use_helper_path(app.path().resource_dir().ok().as_deref()) {
            sidecar = sidecar.env(tidebreak_host_broker::HELPER_PATH_ENV, helper);
        }

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
        self.exchange_before(request, None).await
    }

    async fn exchange_before(
        &mut self,
        request: SidecarRequest,
        dispatch_deadline: Option<Instant>,
    ) -> Result<ExchangeResult, BrokerClientError> {
        let stdin = self.stdin.as_mut().ok_or(BrokerClientError::Closed)?;
        exchange_with_io(request, stdin, &mut self.stdout, dispatch_deadline).await
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

async fn exchange_with_io(
    request: SidecarRequest,
    stdin: &mut (impl AsyncWrite + Unpin),
    stdout: &mut (impl AsyncBufRead + Unpin),
    dispatch_deadline: Option<Instant>,
) -> Result<ExchangeResult, BrokerClientError> {
    if dispatch_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(BrokerClientError::DispatchExpired);
    }
    let confirmed_native_input = matches!(
        &request,
        SidecarRequest::Control(ControlEnvelope {
            request: ControlRequest::CuConfirmControlAction(_),
            ..
        })
    );
    let (expected_channel, expected_id) = match &request {
        SidecarRequest::Control(envelope) => (Channel::Control, envelope.request_id),
        SidecarRequest::Operation(envelope) => (Channel::Operation, envelope.request_id),
    };
    let mut encoded = serde_json::to_vec(&request).map_err(|_| BrokerClientError::Protocol)?;
    if encoded.len() > MAX_REQUEST_BYTES {
        return Err(BrokerClientError::Protocol);
    }
    encoded.push(b'\n');
    let write = async {
        stdin
            .write_all(&encoded)
            .await
            .map_err(|_| BrokerClientError::Closed)?;
        stdin.flush().await.map_err(|_| BrokerClientError::Closed)
    };
    let read = async {
        let frame = read_frame(stdout).await?;
        let response: SidecarResponse =
            serde_json::from_slice(&frame).map_err(|_| BrokerClientError::Protocol)?;
        decode_response(response, expected_channel, expected_id)
    };
    if confirmed_native_input {
        // A confirmed action may leave keys or buttons pressed. Bound admission
        // separately so the broker stays alive for the helper's full cleanup
        // window after this single frame has been sent. Never replay the frame.
        match dispatch_deadline {
            Some(deadline) => timeout_at(deadline, write)
                .await
                .map_err(|_| BrokerClientError::DispatchExpired)?,
            None => timeout(REQUEST_TIMEOUT, write)
                .await
                .map_err(|_| BrokerClientError::Timeout)?,
        }?;
        return timeout(REQUEST_TIMEOUT, read)
            .await
            .map_err(|_| BrokerClientError::Timeout)
            .and_then(|result| result);
    }
    let exchange = async {
        write.await?;
        read.await
    };
    match dispatch_deadline {
        Some(deadline) => timeout_at(deadline, exchange)
            .await
            .map_err(|_| BrokerClientError::DispatchExpired)
            .and_then(|result| result),
        None => timeout(REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_| BrokerClientError::Timeout)
            .and_then(|result| result),
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

/// Resolve only app-owned paths. A harness cannot select the privileged helper.
fn native_cancel_path(data_dir: &Path) -> PathBuf {
    data_dir.join("computer-use-control").join("generation")
}

fn write_native_generation(path: &Path, generation: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let directory = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing control directory"))?;
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let temporary = directory.join(format!(".generation-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(generation.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

#[cfg(target_os = "macos")]
pub(crate) fn computer_use_helper_path(resource_dir: Option<&Path>) -> Option<PathBuf> {
    let packaged = resource_dir.map(|dir| dir.join("host-broker/tidebreak-cu-helper"));
    if let Some(path) = packaged.filter(|path| path.is_file()) {
        return Some(path);
    }
    // Tauri dev runs outside a bundle, after prepare-sidecar stages the helper.
    #[cfg(debug_assertions)]
    {
        let staged =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/host-broker/tidebreak-cu-helper");
        if staged.is_file() {
            return Some(staged);
        }
    }
    None
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
    #[error("host broker is paused while Tidebreak updates")]
    Quiesced,
    #[error("host broker is unavailable until Tidebreak restarts after a failed update")]
    UpdateRecovery,
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
    #[test]
    fn response_deadline_allows_helper_cancellation_and_release() {
        assert!(
            super::REQUEST_TIMEOUT
                >= tidebreak_host_broker::computer_use::HELPER_MANAGED_TIMEOUT
                    + std::time::Duration::from_secs(5)
        );
    }

    use tidebreak_host_broker::{
        sidecar::{SidecarResponse, TransportError, TransportErrorCode},
        ControlResponseEnvelope, ControlResult, HelloResult, Response,
    };

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn confirmed_control_waits_for_cleanup_after_dispatch_deadline_without_replay() {
        let request_id = RequestId::new();
        let request = SidecarRequest::Control(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            request: ControlRequest::CuConfirmControlAction(
                tidebreak_host_broker::CuConfirmControlActionRequest {
                    confirmation_id: uuid::Uuid::new_v4(),
                },
            ),
        });
        let expected_request = serde_json::to_vec(&request).unwrap();
        let (mut client_write, mut server_read) = tokio::io::duplex(4096);
        let (mut server_write, client_read) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            use tokio::io::AsyncReadExt as _;
            let mut reader = BufReader::new(&mut server_read);
            assert_eq!(read_frame(&mut reader).await.unwrap(), expected_request);
            tokio::time::sleep(MUTATION_DISPATCH_WINDOW + Duration::from_secs(1)).await;
            let response = SidecarResponse::Control(ControlResponseEnvelope {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                response: Response::Error(tidebreak_host_broker::ErrorResponse {
                    code: ErrorCode::Internal,
                    message: "input cleanup completed".into(),
                    retryable: false,
                }),
            });
            let mut encoded = serde_json::to_vec(&response).unwrap();
            encoded.push(b'\n');
            server_write.write_all(&encoded).await.unwrap();
            let mut extra = Vec::new();
            reader.read_to_end(&mut extra).await.unwrap();
            assert!(extra.is_empty(), "confirmed control must never replay");
        });
        let started = Instant::now();
        let result = exchange_with_io(
            request,
            &mut client_write,
            &mut BufReader::new(client_read),
            Some(started + MUTATION_DISPATCH_WINDOW),
        )
        .await;
        assert!(
            matches!(result, Err(BrokerClientError::Broker { ref message, .. }) if message == "input cleanup completed"),
            "confirmation must receive the cleanup response, not expire at dispatch deadline"
        );
        assert!(Instant::now() > started + MUTATION_DISPATCH_WINDOW);
        drop(client_write);
        server.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn expired_confirmation_is_not_dispatched() {
        use tokio::io::AsyncReadExt as _;
        let request = SidecarRequest::Control(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            request: ControlRequest::CuConfirmControlAction(
                tidebreak_host_broker::CuConfirmControlActionRequest {
                    confirmation_id: uuid::Uuid::new_v4(),
                },
            ),
        });
        let (mut write, mut peer) = tokio::io::duplex(4096);
        let result = exchange_with_io(
            request,
            &mut write,
            &mut BufReader::new(tokio::io::empty()),
            Some(Instant::now()),
        )
        .await;
        assert!(matches!(result, Err(BrokerClientError::DispatchExpired)));
        drop(write);
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty(), "expired confirmation must send no frame");
    }

    #[tokio::test(start_paused = true)]
    async fn confirmed_control_response_still_has_a_cleanup_bound() {
        let request = SidecarRequest::Control(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            request: ControlRequest::CuConfirmControlAction(
                tidebreak_host_broker::CuConfirmControlActionRequest {
                    confirmation_id: uuid::Uuid::new_v4(),
                },
            ),
        });
        let (mut write, _peer) = tokio::io::duplex(4096);
        let (_server, read) = tokio::io::duplex(4096);
        let started = Instant::now();
        let result = exchange_with_io(
            request,
            &mut write,
            &mut BufReader::new(read),
            Some(started + MUTATION_DISPATCH_WINDOW),
        )
        .await;
        assert!(matches!(result, Err(BrokerClientError::Timeout)));
        assert_eq!(Instant::now() - started, REQUEST_TIMEOUT);
    }

    #[test]
    fn native_stop_and_resume_replace_the_generation() {
        let directory = tempfile::tempdir().unwrap();
        let path = native_cancel_path(directory.path());
        let first = uuid::Uuid::new_v4().to_string();
        write_native_generation(&path, &first).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        write_native_generation(&path, "stopped").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "stopped");
        let resumed = uuid::Uuid::new_v4().to_string();
        write_native_generation(&path, &resumed).unwrap();
        assert_ne!(first, resumed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), resumed);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn quiesce_closes_admission_before_its_queue_barrier() {
        let (commands, mut receiver) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
        let client = BrokerClient {
            commands,
            admission: StdMutex::new(BrokerAdmission::Running),
            task: StdMutex::new(None),
            native_cancel_path: PathBuf::new(),
        };
        let (first_reply, _first_finished) = oneshot::channel();
        client
            .admit(BrokerCommand::Shutdown { reply: first_reply })
            .unwrap();
        client
            .begin_transition(BrokerAdmission::Running, BrokerAdmission::Quiescing)
            .unwrap();
        let (barrier_reply, _barrier_finished) = oneshot::channel();
        client
            .commands
            .try_send(BrokerCommand::Quiesce {
                reply: barrier_reply,
            })
            .unwrap();
        let (late_reply, _late_finished) = oneshot::channel();
        assert!(matches!(
            client.admit(BrokerCommand::Shutdown { reply: late_reply }),
            Err(BrokerClientError::Quiesced)
        ));

        assert!(matches!(
            receiver.try_recv(),
            Ok(BrokerCommand::Shutdown { .. })
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(BrokerCommand::Quiesce { .. })
        ));
        assert!(receiver.try_recv().is_err());

        client.set_admission(BrokerAdmission::Shutdown);
        assert!(!client.finish_transition(BrokerAdmission::Quiescing, BrokerAdmission::Quiesced));
    }

    #[test]
    fn failed_update_recovery_never_retries_into_a_new_sidecar() {
        assert!(!BrokerClientError::UpdateRecovery.retryable_control());
    }

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
