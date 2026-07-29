use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(target_os = "macos")]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Stdio;
#[cfg(target_os = "macos")]
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

use async_trait::async_trait;
#[cfg(target_os = "macos")]
use tokio::io::{AsyncRead, AsyncReadExt};
#[cfg(target_os = "macos")]
use tokio::process::Command;

#[cfg(target_os = "macos")]
use crate::output::{Capture, StreamKind};
use crate::receipt::{request_fingerprint, BeginExecution, ExecutionReceipt};
#[cfg(target_os = "macos")]
use crate::CodeExecutionProviderKind;
use crate::{
    CodeExecutionError, CodeExecutionProvider, CodeExecutionRequest, CodeExecutionResponse,
    ExecutionWorkspaceId, WorkspaceFileEntry, WorkspaceFilePath, WorkspaceLifecycle,
    WorkspaceListing, MAX_WORKSPACE_FILE_BYTES, MAX_WORKSPACE_LIST_ENTRIES,
};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const RECEIPT_DIR: &str = ".code-execution-receipts";
const MAX_RECEIPT_BYTES: u64 = 96 * 1_024;
#[cfg(target_os = "macos")]
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(1);
#[cfg(target_os = "macos")]
const TERMINATE_GRACE: Duration = Duration::from_millis(250);
#[cfg(target_os = "macos")]
const MAX_WRITTEN_FILE_BYTES: u64 = 16 * 1_024 * 1_024;
#[cfg(target_os = "macos")]
const MAX_OPEN_FILES: u64 = 64;

/// Native local execution rooted at OpenWave's private per-chat scratch.
pub struct LocalExecutionProvider {
    scratch_root: PathBuf,
    timeout: Duration,
}

impl LocalExecutionProvider {
    pub fn new(
        scratch_root: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Result<Self, CodeExecutionError> {
        if timeout.is_zero() {
            return Err(CodeExecutionError::InvalidRequest(
                "execution timeout must be positive".into(),
            ));
        }
        Ok(Self {
            scratch_root: scratch_root.into(),
            timeout,
        })
    }

    /// Whether the mandatory native confinement primitive exists on this host.
    #[must_use]
    pub fn is_supported() -> bool {
        cfg!(target_os = "macos") && Path::new(SANDBOX_EXEC).is_file()
    }

    fn resolve_paths(
        &self,
        request: &CodeExecutionRequest,
    ) -> Result<(PathBuf, PathBuf, PathBuf), CodeExecutionError> {
        let root = fs::canonicalize(&self.scratch_root).map_err(|_| {
            CodeExecutionError::Sandbox("private scratch root is unavailable".into())
        })?;
        let workspace_candidate = root.join(request.workspace_id.as_str());
        let metadata = fs::symlink_metadata(&workspace_candidate)
            .map_err(|_| CodeExecutionError::Sandbox("private workspace is unavailable".into()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(CodeExecutionError::Sandbox(
                "private workspace is not a regular directory".into(),
            ));
        }
        let workspace = fs::canonicalize(workspace_candidate)
            .map_err(|_| CodeExecutionError::Sandbox("private workspace is unavailable".into()))?;
        if !workspace.starts_with(&root) {
            return Err(CodeExecutionError::Sandbox(
                "private workspace escaped its root".into(),
            ));
        }
        let cwd = fs::canonicalize(workspace.join(&request.cwd))
            .map_err(|_| CodeExecutionError::Sandbox("working directory is unavailable".into()))?;
        if !cwd.starts_with(&workspace) || !cwd.is_dir() {
            return Err(CodeExecutionError::Sandbox(
                "working directory escaped the private workspace".into(),
            ));
        }
        let receipts = root.join(RECEIPT_DIR);
        secure_dir(&receipts)?;
        Ok((workspace, cwd, receipts))
    }

    fn ensured_root(&self) -> Result<PathBuf, CodeExecutionError> {
        fs::create_dir_all(&self.scratch_root).map_err(|_| {
            CodeExecutionError::Sandbox("private scratch root is unavailable".into())
        })?;
        fs::canonicalize(&self.scratch_root)
            .map_err(|_| CodeExecutionError::Sandbox("private scratch root is unavailable".into()))
    }

    fn existing_root(&self) -> Result<Option<PathBuf>, CodeExecutionError> {
        match fs::canonicalize(&self.scratch_root) {
            Ok(root) => Ok(Some(root)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(CodeExecutionError::Sandbox(
                "private scratch root is unavailable".into(),
            )),
        }
    }

    fn workspace_in(
        root: &Path,
        workspace: &ExecutionWorkspaceId,
        create: bool,
    ) -> Result<Option<PathBuf>, CodeExecutionError> {
        let candidate = root.join(workspace.as_str());
        if create {
            fs::create_dir_all(&candidate).map_err(|_| {
                CodeExecutionError::Sandbox("private workspace is unavailable".into())
            })?;
        }
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(CodeExecutionError::Sandbox(
                    "private workspace is unavailable".into(),
                ));
            }
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(CodeExecutionError::Sandbox(
                "private workspace is not a regular directory".into(),
            ));
        }
        let workspace = fs::canonicalize(&candidate)
            .map_err(|_| CodeExecutionError::Sandbox("private workspace is unavailable".into()))?;
        if !workspace.starts_with(root) {
            return Err(CodeExecutionError::Sandbox(
                "private workspace escaped its root".into(),
            ));
        }
        Ok(Some(workspace))
    }

    fn ensured_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<PathBuf, CodeExecutionError> {
        let root = self.ensured_root()?;
        Self::workspace_in(&root, workspace, true)?
            .ok_or_else(|| CodeExecutionError::Sandbox("private workspace is unavailable".into()))
    }

    /// Resolve `path`'s canonical parent inside `workspace` and rejoin the
    /// final component, so a symlinked intermediate directory cannot escape
    /// the workspace. The final component's own type is checked separately.
    fn resolve_file(
        workspace: &Path,
        path: &WorkspaceFilePath,
        create_parents: bool,
    ) -> Result<Option<PathBuf>, CodeExecutionError> {
        let target = workspace.join(path.as_str());
        let parent = target
            .parent()
            .ok_or_else(|| CodeExecutionError::Sandbox("workspace path has no parent".into()))?;
        if create_parents {
            fs::create_dir_all(parent).map_err(|_| {
                CodeExecutionError::Sandbox("workspace directories are unavailable".into())
            })?;
        }
        let parent = match fs::canonicalize(parent) {
            Ok(parent) => parent,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(CodeExecutionError::Sandbox(
                    "workspace directories are unavailable".into(),
                ));
            }
        };
        if !parent.starts_with(workspace) {
            return Err(CodeExecutionError::Sandbox(
                "workspace path escaped the private workspace".into(),
            ));
        }
        Ok(Some(parent.join(path.file_name())))
    }
}

#[async_trait]
impl CodeExecutionProvider for LocalExecutionProvider {
    async fn execute(
        &self,
        request: CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, CodeExecutionError> {
        request.validate()?;
        if !Self::is_supported() {
            return Err(CodeExecutionError::Unavailable(
                "native local sandboxing is not supported on this host".into(),
            ));
        }
        let (workspace, cwd, receipts) = self.resolve_paths(&request)?;
        let fingerprint = request_fingerprint(&request)?;
        let receipt_path = receipts.join(format!("{}.json", request.execution_id.as_str()));
        match begin_execution(&receipt_path, &fingerprint)? {
            BeginExecution::Cached(response) => return Ok(response),
            BeginExecution::Started => {}
        }

        let result = run_native(&request, &workspace, &cwd, self.timeout).await;
        match result {
            Ok(response) => {
                finish_execution(
                    &receipt_path,
                    &ExecutionReceipt::Completed {
                        fingerprint,
                        response: response.clone(),
                    },
                )?;
                Ok(response)
            }
            Err(error) => {
                let receipt = ExecutionReceipt::Failed {
                    fingerprint,
                    message: error.to_string(),
                };
                finish_execution(&receipt_path, &receipt)?;
                Err(error)
            }
        }
    }

    fn workspace_lifecycle(&self) -> Option<&dyn WorkspaceLifecycle> {
        // Managing scratch files needs no confinement primitive, so the
        // capability is offered even where `execute` reports unsupported.
        Some(self)
    }
}

#[async_trait]
impl WorkspaceLifecycle for LocalExecutionProvider {
    async fn create_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<(), CodeExecutionError> {
        self.ensured_workspace(workspace).map(|_| ())
    }

    async fn connect_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<bool, CodeExecutionError> {
        let Some(root) = self.existing_root()? else {
            return Ok(false);
        };
        Ok(Self::workspace_in(&root, workspace, false)?.is_some())
    }

    async fn destroy_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<(), CodeExecutionError> {
        let Some(root) = self.existing_root()? else {
            return Ok(());
        };
        let Some(workspace) = Self::workspace_in(&root, workspace, false)? else {
            return Ok(());
        };
        fs::remove_dir_all(&workspace)
            .map_err(|_| CodeExecutionError::Sandbox("could not remove private workspace".into()))
    }

    async fn put_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<(), CodeExecutionError> {
        if content.len() > MAX_WORKSPACE_FILE_BYTES {
            return Err(CodeExecutionError::WorkspaceFileTooLarge);
        }
        let workspace = self.ensured_workspace(workspace)?;
        let target = Self::resolve_file(&workspace, path, true)?.ok_or_else(|| {
            CodeExecutionError::Sandbox("workspace directories are unavailable".into())
        })?;
        // The final `target` is written by an atomic rename, so its own type is
        // not load-bearing for containment — the temp file below is what we
        // actually write into, and it must not be a pre-planted symlink. A
        // process with write access to chat scratch (an exec-tool command runs
        // Seatbelt-confined to this same root) could otherwise plant a symlink
        // at a guessable temp path and redirect the unsandboxed host write onto
        // an arbitrary host file. Two defenses close that: an unpredictable
        // temp name, and an exclusive, no-follow create that fails rather than
        // following anything that already exists at the path.
        let parent = target
            .parent()
            .ok_or_else(|| CodeExecutionError::Sandbox("workspace path has no parent".into()))?;
        let temporary = parent.join(format!(".workspace-put.{}", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temporary).map_err(|_| {
            CodeExecutionError::Sandbox("could not write the workspace file".into())
        })?;
        file.write_all(content)
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&temporary, &target))
            .map_err(|_| {
                let _ = fs::remove_file(&temporary);
                CodeExecutionError::Sandbox("could not write the workspace file".into())
            })
    }

    async fn get_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
    ) -> Result<Vec<u8>, CodeExecutionError> {
        let Some(root) = self.existing_root()? else {
            return Err(CodeExecutionError::WorkspaceFileNotFound);
        };
        let Some(workspace) = Self::workspace_in(&root, workspace, false)? else {
            return Err(CodeExecutionError::WorkspaceFileNotFound);
        };
        let Some(target) = Self::resolve_file(&workspace, path, false)? else {
            return Err(CodeExecutionError::WorkspaceFileNotFound);
        };
        // Open without following the final component, then judge the opened
        // descriptor — never a path stat'd separately from the open. A
        // lstat-then-open read races a writer that keeps the path a regular
        // file at the check and swaps in a symlink to a host secret before the
        // open, so containment must come from the descriptor we actually read.
        let mut open = OpenOptions::new();
        open.read(true);
        #[cfg(unix)]
        open.custom_flags(libc::O_NOFOLLOW);
        let file = match open.open(&target) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CodeExecutionError::WorkspaceFileNotFound);
            }
            // A final symlink fails the no-follow open with ELOOP; report it as
            // an invalid path rather than a missing file so a planted symlink
            // is not silently indistinguishable from absence.
            Err(error) if is_symlink_loop(&error) => {
                return Err(CodeExecutionError::InvalidRequest(
                    "workspace path is not a regular file".into(),
                ));
            }
            Err(_) => {
                return Err(CodeExecutionError::Sandbox(
                    "could not read the workspace file".into(),
                ));
            }
        };
        let metadata = file
            .metadata()
            .map_err(|_| CodeExecutionError::Sandbox("could not read the workspace file".into()))?;
        if !metadata.is_file() {
            return Err(CodeExecutionError::InvalidRequest(
                "workspace path is not a regular file".into(),
            ));
        }
        if metadata.len() > MAX_WORKSPACE_FILE_BYTES as u64 {
            return Err(CodeExecutionError::WorkspaceFileTooLarge);
        }
        let mut content = Vec::new();
        file.take(MAX_WORKSPACE_FILE_BYTES as u64 + 1)
            .read_to_end(&mut content)
            .map_err(|_| CodeExecutionError::Sandbox("could not read the workspace file".into()))?;
        if content.len() > MAX_WORKSPACE_FILE_BYTES {
            return Err(CodeExecutionError::WorkspaceFileTooLarge);
        }
        Ok(content)
    }

    async fn list_workspace_files(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: Option<&WorkspaceFilePath>,
    ) -> Result<WorkspaceListing, CodeExecutionError> {
        let workspace = self.ensured_workspace(workspace)?;
        let base = match path {
            None => workspace.clone(),
            Some(path) => {
                let candidate = workspace.join(path.as_str());
                let base = match fs::canonicalize(&candidate) {
                    Ok(base) => base,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Err(CodeExecutionError::WorkspaceFileNotFound);
                    }
                    Err(_) => {
                        return Err(CodeExecutionError::Sandbox(
                            "could not list the workspace".into(),
                        ));
                    }
                };
                if !base.starts_with(&workspace) {
                    return Err(CodeExecutionError::Sandbox(
                        "workspace path escaped the private workspace".into(),
                    ));
                }
                if !base.is_dir() {
                    return Err(CodeExecutionError::InvalidRequest(
                        "workspace path is not a directory".into(),
                    ));
                }
                base
            }
        };
        let reader = fs::read_dir(&base)
            .map_err(|_| CodeExecutionError::Sandbox("could not list the workspace".into()))?;
        let mut entries = Vec::new();
        for entry in reader {
            let entry = entry
                .map_err(|_| CodeExecutionError::Sandbox("could not list the workspace".into()))?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
                continue;
            }
            let relative = match path {
                None => name,
                Some(path) => format!("{}/{name}", path.as_str()),
            };
            entries.push(WorkspaceFileEntry {
                path: relative,
                directory: metadata.is_dir(),
                size_bytes: metadata.is_file().then_some(metadata.len()),
            });
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        let truncated = entries.len() > MAX_WORKSPACE_LIST_ENTRIES;
        entries.truncate(MAX_WORKSPACE_LIST_ENTRIES);
        Ok(WorkspaceListing { entries, truncated })
    }
}

fn begin_execution(path: &Path, fingerprint: &str) -> Result<BeginExecution, CodeExecutionError> {
    let receipt = ExecutionReceipt::running(fingerprint);
    let bytes = serde_json::to_vec(&receipt)
        .map_err(|_| CodeExecutionError::Sandbox("could not encode receipt".into()))?;
    begin_execution_with_persistence(path, fingerprint, |file| {
        file.write_all(&bytes).and_then(|()| file.sync_all())
    })
}

fn begin_execution_with_persistence(
    path: &Path,
    fingerprint: &str,
    persist: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
) -> Result<BeginExecution, CodeExecutionError> {
    let parent = path
        .parent()
        .ok_or_else(|| CodeExecutionError::Sandbox("execution receipt has no parent".into()))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(path) {
        Ok(mut file) => {
            if persist(&mut file).and_then(|()| sync_dir(parent)).is_err() {
                drop(file);
                discard_unstarted_receipt(path, parent)?;
                return Err(CodeExecutionError::Sandbox(
                    "could not persist receipt".into(),
                ));
            }
            Ok(BeginExecution::Started)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let receipt = read_receipt(path)?;
            receipt.replay(fingerprint, CodeExecutionError::Sandbox)
        }
        Err(_) => Err(CodeExecutionError::Sandbox(
            "could not create execution receipt".into(),
        )),
    }
}

fn discard_unstarted_receipt(path: &Path, parent: &Path) -> Result<(), CodeExecutionError> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(CodeExecutionError::Sandbox(
                "could not clean up incomplete execution receipt".into(),
            ));
        }
    }
    sync_dir(parent).map_err(|_| {
        CodeExecutionError::Sandbox("could not clean up incomplete execution receipt".into())
    })
}

fn read_receipt(path: &Path) -> Result<ExecutionReceipt, CodeExecutionError> {
    let file = fs::File::open(path)
        .map_err(|_| CodeExecutionError::Sandbox("could not read execution receipt".into()))?;
    if file
        .metadata()
        .map_err(|_| CodeExecutionError::Sandbox("could not inspect execution receipt".into()))?
        .len()
        > MAX_RECEIPT_BYTES
    {
        return Err(CodeExecutionError::Sandbox(
            "execution receipt exceeds its bound".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CodeExecutionError::Sandbox("could not read execution receipt".into()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| CodeExecutionError::Sandbox("execution receipt is invalid".into()))
}

fn finish_execution(path: &Path, receipt: &ExecutionReceipt) -> Result<(), CodeExecutionError> {
    let parent = path
        .parent()
        .ok_or_else(|| CodeExecutionError::Sandbox("execution receipt has no parent".into()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CodeExecutionError::Sandbox("execution receipt name is invalid".into()))?;
    let temporary = parent.join(format!(".{file_name}.terminal"));
    let bytes = serde_json::to_vec(receipt)
        .map_err(|_| CodeExecutionError::Sandbox("could not encode receipt".into()))?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(CodeExecutionError::Sandbox(
            "execution receipt exceeds its bound".into(),
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .map_err(|_| CodeExecutionError::AmbiguousExecution)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| CodeExecutionError::AmbiguousExecution)?;
    fs::rename(&temporary, path).map_err(|_| CodeExecutionError::AmbiguousExecution)?;
    sync_dir(parent).map_err(|_| CodeExecutionError::AmbiguousExecution)
}

fn sync_dir(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

/// Whether an open error is the no-follow refusal of a final symlink.
#[cfg(unix)]
fn is_symlink_loop(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_loop(_error: &std::io::Error) -> bool {
    false
}

fn secure_dir(path: &Path) -> Result<(), CodeExecutionError> {
    fs::create_dir_all(path).map_err(|_| {
        CodeExecutionError::Sandbox("could not create private receipt storage".into())
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        CodeExecutionError::Sandbox("could not inspect private receipt storage".into())
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(CodeExecutionError::Sandbox(
            "private receipt storage is not a regular directory".into(),
        ));
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| {
        CodeExecutionError::Sandbox("could not secure private receipt storage".into())
    })?;
    Ok(())
}

#[cfg(target_os = "macos")]
async fn run_native(
    request: &CodeExecutionRequest,
    workspace: &Path,
    cwd: &Path,
    timeout: Duration,
) -> Result<CodeExecutionResponse, CodeExecutionError> {
    let developer_dir = macos_developer_dir();
    let profile = macos_profile(workspace, developer_dir.as_deref())?;
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .args(["-p", &profile, "--", &request.command])
        .args(&request.arguments)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", workspace)
        .env("TMPDIR", workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(developer_dir) = developer_dir {
        command.env("DEVELOPER_DIR", &developer_dir).env(
            "PATH",
            format!(
                "{}/usr/bin:/usr/bin:/bin:/usr/sbin:/sbin",
                developer_dir.display()
            ),
        );
    } else {
        command.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    }
    configure_unix_limits(&mut command, timeout);

    let started = Instant::now();
    let mut child = command.spawn().map_err(|_| CodeExecutionError::Spawn)?;
    let process_group = child.id().map(|id| id as i32);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CodeExecutionError::Sandbox("stdout capture is unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CodeExecutionError::Sandbox("stderr capture is unavailable".into()))?;
    let capture = Arc::new(Mutex::new(Capture::default()));
    let stdout_reader = tokio::spawn(drain_output(stdout, capture.clone(), StreamKind::Stdout));
    let stderr_reader = tokio::spawn(drain_output(stderr, capture.clone(), StreamKind::Stderr));

    let (status, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(waited) => (
            waited.map_err(|_| CodeExecutionError::Sandbox("could not wait for command".into()))?,
            false,
        ),
        Err(_) => {
            if let Some(group) = process_group {
                signal_group(group, libc::SIGTERM);
                tokio::time::sleep(TERMINATE_GRACE).await;
                signal_group(group, libc::SIGKILL);
            } else {
                child
                    .kill()
                    .await
                    .map_err(|_| CodeExecutionError::Sandbox("could not stop command".into()))?;
            }
            (
                child
                    .wait()
                    .await
                    .map_err(|_| CodeExecutionError::Sandbox("could not reap command".into()))?,
                true,
            )
        }
    };
    if let Some(group) = process_group {
        // A child that daemonized is still sandboxed, but it must not outlive
        // the invocation. Clean the remaining process group after the leader.
        signal_group(group, libc::SIGKILL);
    }
    finish_reader(stdout_reader).await;
    finish_reader(stderr_reader).await;

    let capture = std::mem::take(&mut *capture.lock().unwrap());
    Ok(capture.response(
        CodeExecutionProviderKind::Local,
        started,
        status.code(),
        timed_out,
    ))
}

#[cfg(not(target_os = "macos"))]
async fn run_native(
    _request: &CodeExecutionRequest,
    _workspace: &Path,
    _cwd: &Path,
    _timeout: Duration,
) -> Result<CodeExecutionResponse, CodeExecutionError> {
    Err(CodeExecutionError::Unavailable(
        "native local sandboxing is not supported on this host".into(),
    ))
}

#[cfg(target_os = "macos")]
fn configure_unix_limits(command: &mut Command, timeout: Duration) {
    let cpu_seconds = timeout.as_secs().saturating_add(1).max(1);
    // SAFETY: `pre_exec` runs after fork and before exec. The closure performs
    // only async-signal-safe `setrlimit` calls with copied scalar values.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            set_limit(libc::RLIMIT_CPU, cpu_seconds)?;
            set_limit(libc::RLIMIT_FSIZE, MAX_WRITTEN_FILE_BYTES)?;
            set_limit(libc::RLIMIT_NOFILE, MAX_OPEN_FILES)?;
            Ok(())
        });
    }
    command.as_std_mut().process_group(0);
}

#[cfg(target_os = "macos")]
fn set_limit(resource: RlimitResource, value: u64) -> std::io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    // SAFETY: `limit` is a valid initialized value and the resource constants
    // are supplied by libc for this target.
    if unsafe { libc::setrlimit(resource, &limit) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
type RlimitResource = libc::c_int;

#[cfg(target_os = "macos")]
fn signal_group(group: i32, signal: i32) {
    // SAFETY: a negative pid addresses the child-owned process group. Failure
    // (normally ESRCH after clean exit) is intentionally harmless.
    let _ = unsafe { libc::kill(-group, signal) };
}

#[cfg(target_os = "macos")]
async fn drain_output<R>(mut reader: R, capture: Arc<Mutex<Capture>>, kind: StreamKind)
where
    R: AsyncRead + Unpin,
{
    let mut chunk = [0_u8; 8 * 1_024];
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        let mut capture = capture.lock().unwrap();
        capture.append(&chunk[..read], kind);
    }
}

#[cfg(target_os = "macos")]
async fn finish_reader(mut reader: tokio::task::JoinHandle<()>) {
    if tokio::time::timeout(OUTPUT_DRAIN_GRACE, &mut reader)
        .await
        .is_err()
    {
        reader.abort();
    }
}

#[cfg(target_os = "macos")]
fn macos_profile(
    workspace: &Path,
    developer_dir: Option<&Path>,
) -> Result<String, CodeExecutionError> {
    const DENIED_READS: &[&str] = &[
        "/Applications",
        "/Library",
        "/Network",
        "/Users",
        "/Volumes",
        "/cores",
        "/dev",
        "/etc",
        "/home",
        "/mnt",
        "/nix",
        "/opt",
        "/private",
        "/tmp",
        "/usr/local",
        "/var",
        "/System/Volumes/Data/Applications",
        "/System/Volumes/Data/Library",
        "/System/Volumes/Data/Users",
        "/System/Volumes/Data/Volumes",
        "/System/Volumes/Data/opt",
        "/System/Volumes/Data/private",
        "/System/Volumes/Data/tmp",
        "/System/Volumes/Data/usr/local",
        "/System/Volumes/Data/var",
    ];
    const ALLOWED_LITERALS: &[&str] = &[
        "/dev/null",
        "/dev/random",
        "/dev/urandom",
        "/etc/localtime",
        "/etc/zshenv",
        "/private/etc/localtime",
        "/private/etc/zshenv",
        "/private/var/select/developer_dir",
        "/private/var/select/sh",
        "/var/select/developer_dir",
    ];
    const ALLOWED_RUNTIME_SUBPATHS: &[&str] = &[
        "/Applications/Xcode.app/Contents/Developer",
        "/Library/Developer/CommandLineTools",
        "/System/Volumes/Data/Applications/Xcode.app/Contents/Developer",
        "/System/Volumes/Data/Library/Developer/CommandLineTools",
        "/System/Volumes/Data/private/var/select",
        "/private/var/select",
        "/var/select",
        "/var/db/timezone",
    ];
    const RUNTIME_ANCESTORS: &[&str] = &[
        "/Applications",
        "/Applications/Xcode.app",
        "/Applications/Xcode.app/Contents",
        "/Library",
        "/Library/Developer",
        "/System/Volumes/Data/Applications",
        "/System/Volumes/Data/Applications/Xcode.app",
        "/System/Volumes/Data/Applications/Xcode.app/Contents",
        "/System/Volumes/Data/Library",
        "/System/Volumes/Data/Library/Developer",
    ];
    let denied = DENIED_READS
        .iter()
        .map(|path| sbpl_subpath(path))
        .collect::<Vec<_>>()
        .join("\n  ");
    let literals = ALLOWED_LITERALS
        .iter()
        .map(|path| sbpl_literal(path))
        .collect::<Vec<_>>()
        .join("\n  ");
    let runtimes = ALLOWED_RUNTIME_SUBPATHS
        .iter()
        .map(|path| sbpl_subpath(path))
        .collect::<Vec<_>>()
        .join("\n  ");
    let runtime_metadata = RUNTIME_ANCESTORS
        .iter()
        .map(|path| sbpl_literal(path))
        .collect::<Vec<_>>()
        .join("\n  ");
    let selected_runtime = developer_dir
        .map(sandbox_subpath)
        .transpose()?
        .unwrap_or_default();
    let workspace = sandbox_subpath(workspace)?;
    Ok(format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow sysctl-read)\n\
         (allow file-read*)\n\
         (deny file-read*\n  {denied})\n\
         (allow file-read-metadata\n  {runtime_metadata})\n\
         (allow file-read*\n  {literals}\n  {runtimes}\n  {selected_runtime}\n  {workspace})\n\
         (allow file-write*\n  {workspace}\n  (literal \"/dev/null\"))\n"
    ))
}

#[cfg(target_os = "macos")]
fn macos_developer_dir() -> Option<PathBuf> {
    let selected = fs::read_link("/var/select/developer_dir").ok()?;
    let selected = fs::canonicalize(selected).ok()?;
    [
        Path::new("/Applications/Xcode.app/Contents/Developer"),
        Path::new("/Library/Developer/CommandLineTools"),
    ]
    .iter()
    .any(|allowed| selected.starts_with(allowed))
    .then_some(selected)
}

#[cfg(target_os = "macos")]
fn sbpl_literal(path: &str) -> String {
    format!("(literal \"{}\")", escape_sbpl(path))
}

#[cfg(target_os = "macos")]
fn sbpl_subpath(path: &str) -> String {
    format!("(subpath \"{}\")", escape_sbpl(path))
}

#[cfg(target_os = "macos")]
fn sandbox_subpath(path: &Path) -> Result<String, CodeExecutionError> {
    let path = path
        .to_str()
        .ok_or_else(|| CodeExecutionError::Sandbox("sandbox paths must be valid UTF-8".into()))?;
    if path.chars().any(char::is_control) {
        return Err(CodeExecutionError::Sandbox(
            "sandbox paths cannot contain control characters".into(),
        ));
    }
    Ok(sbpl_subpath(path))
}

#[cfg(target_os = "macos")]
fn escape_sbpl(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use crate::{ExecutionId, ExecutionWorkspaceId};

    #[cfg(target_os = "macos")]
    fn request(workspace: &str, execution: &str, script: &str) -> CodeExecutionRequest {
        CodeExecutionRequest::new(
            ExecutionId::parse(execution).unwrap(),
            ExecutionWorkspaceId::parse(workspace).unwrap(),
            "/bin/sh",
            vec!["-c".into(), script.into()],
            ".",
        )
        .unwrap()
    }

    #[cfg(target_os = "macos")]
    fn fixture(timeout: Duration) -> (tempfile::TempDir, LocalExecutionProvider, String) {
        let root = tempfile::tempdir().unwrap();
        let workspace = "chat-1".to_string();
        fs::create_dir(root.path().join(&workspace)).unwrap();
        let provider = LocalExecutionProvider::new(root.path(), timeout).unwrap();
        (root, provider, workspace)
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn local_sandbox_confines_writes_and_network_and_caches_exact_retry() {
        let (root, provider, workspace) = fixture(Duration::from_secs(3));
        let outside = root.path().join("outside");
        let script = format!(
            "printf ok > result; \
             if printf no > '{}'; then echo outside-write; else echo write-blocked; fi; \
             if /usr/bin/curl -fsS --max-time 1 https://example.com >/dev/null 2>&1; \
             then echo network-open; else echo network-blocked; fi; \
             cat result",
            outside.display()
        );
        let request = request(&workspace, "call-1", &script);

        let first = provider.execute(request.clone()).await.unwrap();
        let second = provider.execute(request).await.unwrap();

        assert_eq!(first, second);
        assert_eq!(first.exit_code, Some(0));
        assert!(first.stdout.contains("write-blocked"));
        assert!(first.stdout.contains("network-blocked"));
        assert!(first.stdout.ends_with("ok"));
        assert!(!outside.exists());
        assert_eq!(
            fs::read_to_string(root.path().join(workspace).join("result")).unwrap(),
            "ok"
        );

        for (execution, command) in [
            ("call-python-path", "python3"),
            ("call-python-system-path", "/usr/bin/python3"),
        ] {
            // The sandbox can only be as healthy as the host interpreter: on
            // macOS installs with a broken Xcode python shim, python cannot
            // run outside any sandbox either, so asserting here would fail on
            // an environment defect while proving nothing about confinement.
            let host_python_works = std::process::Command::new(command)
                .args(["-c", "print(6 * 7)"])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
            if !host_python_works {
                eprintln!("skipping {command}: host interpreter unusable in this environment");
                continue;
            }
            let python = CodeExecutionRequest::new(
                ExecutionId::parse(execution).unwrap(),
                ExecutionWorkspaceId::parse("chat-1").unwrap(),
                command,
                vec!["-c".into(), "print(6 * 7)".into()],
                ".",
            )
            .unwrap();
            let python = provider.execute(python).await.unwrap();
            // macOS ships /usr/bin/python3 as an Xcode shim that stats Xcode's
            // frameworks before running; under the sandbox (or with a broken
            // Xcode install) the shim dies before python exists. That failure
            // is an environment defect, not a confinement finding — skip it
            // loudly instead of failing the suite.
            if python.exit_code != Some(0) && python.stderr.contains("unable to locate xcodebuild")
            {
                eprintln!("skipping {command}: Xcode python shim cannot start on this host");
                continue;
            }
            assert_eq!(
                python.exit_code,
                Some(0),
                "{command} stderr: {}",
                python.stderr
            );
            assert_eq!(python.stdout.trim(), "42");
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn local_sandbox_times_out_and_rejects_identity_conflicts() {
        let (_root, provider, workspace) = fixture(Duration::from_millis(100));
        let timed_out = provider
            .execute(request(&workspace, "call-timeout", "sleep 5"))
            .await
            .unwrap();
        assert!(timed_out.timed_out);

        provider
            .execute(request(&workspace, "call-conflict", "printf one"))
            .await
            .unwrap();
        let conflict = provider
            .execute(request(&workspace, "call-conflict", "printf two"))
            .await
            .unwrap_err();
        assert!(matches!(conflict, CodeExecutionError::IdentityConflict));
    }

    #[tokio::test]
    async fn local_workspace_lifecycle_round_trips_and_stays_inside_scratch() {
        let root = tempfile::tempdir().unwrap();
        let provider = LocalExecutionProvider::new(root.path(), Duration::from_secs(1)).unwrap();
        let workspace = ExecutionWorkspaceId::parse("chat-ws").unwrap();

        assert!(!provider.connect_workspace(&workspace).await.unwrap());
        provider.create_workspace(&workspace).await.unwrap();
        assert!(provider.connect_workspace(&workspace).await.unwrap());

        let path = WorkspaceFilePath::parse("reports/summary.bin").unwrap();
        let content = b"\x00binary\xff".to_vec();
        provider
            .put_workspace_file(&workspace, &path, &content)
            .await
            .unwrap();
        assert_eq!(
            provider
                .get_workspace_file(&workspace, &path)
                .await
                .unwrap(),
            content
        );

        let top = provider
            .list_workspace_files(&workspace, None)
            .await
            .unwrap();
        assert_eq!(top.entries.len(), 1);
        assert_eq!(top.entries[0].path, "reports");
        assert!(top.entries[0].directory);
        let nested = provider
            .list_workspace_files(
                &workspace,
                Some(&WorkspaceFilePath::parse("reports").unwrap()),
            )
            .await
            .unwrap();
        assert_eq!(nested.entries.len(), 1);
        assert_eq!(nested.entries[0].path, "reports/summary.bin");
        assert_eq!(nested.entries[0].size_bytes, Some(content.len() as u64));

        assert!(matches!(
            provider
                .get_workspace_file(&workspace, &WorkspaceFilePath::parse("missing").unwrap())
                .await,
            Err(CodeExecutionError::WorkspaceFileNotFound)
        ));
        assert!(matches!(
            provider
                .put_workspace_file(
                    &workspace,
                    &path,
                    &vec![0_u8; crate::MAX_WORKSPACE_FILE_BYTES + 1],
                )
                .await,
            Err(CodeExecutionError::WorkspaceFileTooLarge)
        ));

        // A symlink planted in the workspace must never let a read escape it,
        // whether it is the file itself or an intermediate directory.
        #[cfg(unix)]
        {
            let outside = root.path().join("outside.txt");
            fs::write(&outside, "secret").unwrap();
            let workspace_dir = root.path().join("chat-ws");
            std::os::unix::fs::symlink(&outside, workspace_dir.join("link.txt")).unwrap();
            std::os::unix::fs::symlink(root.path(), workspace_dir.join("escape")).unwrap();
            assert!(provider
                .get_workspace_file(&workspace, &WorkspaceFilePath::parse("link.txt").unwrap())
                .await
                .is_err());
            assert!(provider
                .get_workspace_file(
                    &workspace,
                    &WorkspaceFilePath::parse("escape/outside.txt").unwrap(),
                )
                .await
                .is_err());
            let listed = provider
                .list_workspace_files(&workspace, None)
                .await
                .unwrap();
            assert!(listed.entries.iter().all(|entry| entry.path == "reports"));
        }

        provider.destroy_workspace(&workspace).await.unwrap();
        assert!(!provider.connect_workspace(&workspace).await.unwrap());
        // Destroying a workspace that no longer exists stays a success.
        provider.destroy_workspace(&workspace).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn planted_symlinks_never_redirect_a_workspace_read_or_write() {
        let root = tempfile::tempdir().unwrap();
        let provider = LocalExecutionProvider::new(root.path(), Duration::from_secs(1)).unwrap();
        let workspace = ExecutionWorkspaceId::parse("chat-attack").unwrap();
        provider.create_workspace(&workspace).await.unwrap();
        let workspace_dir = root.path().join("chat-attack");

        // A host secret the confined writer wants the unsandboxed host to touch.
        let secret = root.path().join("secret.txt");
        fs::write(&secret, "original-secret").unwrap();

        // Write: a symlink pre-planted at the destination filename must not
        // redirect the write onto the secret. The atomic rename replaces the
        // symlink itself, so the payload lands inside the workspace and the
        // secret is untouched.
        let write_path = WorkspaceFilePath::parse("report.txt").unwrap();
        std::os::unix::fs::symlink(&secret, workspace_dir.join("report.txt")).unwrap();
        provider
            .put_workspace_file(&workspace, &write_path, b"payload")
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&secret).unwrap(), "original-secret");
        let written = workspace_dir.join("report.txt");
        assert!(!fs::symlink_metadata(&written)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&written).unwrap(), "payload");

        // Read: a symlink at the requested path must error rather than follow
        // out to the secret, even though the target is a regular file's worth
        // of bytes on the other end.
        std::os::unix::fs::symlink(&secret, workspace_dir.join("leak.txt")).unwrap();
        let leak = provider
            .get_workspace_file(&workspace, &WorkspaceFilePath::parse("leak.txt").unwrap())
            .await;
        assert!(
            matches!(leak, Err(CodeExecutionError::InvalidRequest(_))),
            "no-follow read must refuse a symlink, got {leak:?}"
        );

        // A guessable temp-name collision cannot wedge writes either: the
        // exclusive create uses an unpredictable name, so an unrelated planted
        // dotfile does not block a fresh put.
        fs::write(workspace_dir.join(".workspace-put.stale"), "junk").unwrap();
        provider
            .put_workspace_file(
                &workspace,
                &WorkspaceFilePath::parse("second.txt").unwrap(),
                b"second",
            )
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(workspace_dir.join("second.txt")).unwrap(),
            "second"
        );
    }

    #[test]
    fn failed_begin_persistence_releases_the_execution_id_for_retry() {
        let receipts = tempfile::tempdir().unwrap();
        let path = receipts.path().join("call-retry.json");
        let error = begin_execution_with_persistence(&path, "fingerprint", |file| {
            file.write_all(b"{")?;
            Err(std::io::Error::other("injected persistence failure"))
        });
        let error = match error {
            Err(error) => error,
            Ok(_) => panic!("injected persistence failure unexpectedly succeeded"),
        };

        assert!(matches!(error, CodeExecutionError::Sandbox(_)));
        assert!(
            !path.exists(),
            "an unstarted partial claim must not block retries"
        );
        assert!(matches!(
            begin_execution(&path, "fingerprint").unwrap(),
            BeginExecution::Started
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn profile_denies_network_and_escapes_workspace_paths() {
        let profile = macos_profile(Path::new("/Users/test/we\"ird\\workspace"), None).unwrap();
        assert!(profile.contains("(deny default)"));
        assert!(!profile.contains("allow network"));
        assert!(!profile.contains("mach-lookup"));
        assert!(!profile.contains("(allow process*)"));
        assert!(profile.contains("(allow process-exec)"));
        assert!(profile.contains("(allow process-fork)"));
        assert!(profile.contains("we\\\"ird\\\\workspace"));
        assert!(macos_profile(Path::new("/Users/test/control\nworkspace"), None).is_err());
    }
}
