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
use tidebreak_core::NetworkPolicy;
#[cfg(target_os = "macos")]
use tokio::io::{AsyncRead, AsyncReadExt};
#[cfg(target_os = "macos")]
use tokio::process::Command;

use crate::host_paths::{resolve_scratch_directory, ScratchDir, ScratchEntryKind};
#[cfg(target_os = "macos")]
use crate::network::LocalEgressBroker;
#[cfg(target_os = "macos")]
use crate::output::{Capture, StreamKind};
use crate::receipt::{request_fingerprint, BeginExecution, ExecutionReceipt};
use crate::sbpl::SANDBOX_EXEC;
#[cfg(target_os = "macos")]
use crate::ExecProviderKind;
use crate::{
    ExecError, ExecProvider, ExecRequest, ExecResponse, ExecUnavailableReason,
    ExecutionWorkspaceId, WorkspaceFileEntry, WorkspaceFilePath, WorkspaceLifecycle,
    WorkspaceListing, MAX_WORKSPACE_FILE_BYTES, MAX_WORKSPACE_LIST_ENTRIES,
};
#[cfg(target_os = "macos")]
use crate::{ExecFolderAccess, ExecFolderGrant};

const RECEIPT_DIR: &str = ".code-execution-receipts";
const ENV_HOME_DIR: &str = ".code-execution-env-homes";
const MAX_RECEIPT_BYTES: u64 = 96 * 1_024;
#[cfg(target_os = "macos")]
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(1);
#[cfg(target_os = "macos")]
const TERMINATE_GRACE: Duration = Duration::from_millis(250);
#[cfg(target_os = "macos")]
const MAX_WRITTEN_FILE_BYTES: u64 = 16 * 1_024 * 1_024;
#[cfg(target_os = "macos")]
// Roomy enough for interpreters, package installs, and office-format
// libraries that open many archive members at once; still far below the
// system default, so a descriptor leak dies quickly.
const MAX_OPEN_FILES: u64 = 512;

#[cfg(any(target_os = "macos", test))]
const SANDBOX_PATH_DENIED_CODE: &str = "sandbox_path_denied";
#[cfg(target_os = "macos")]
const DENIED_READ_ROOTS: &[&str] = &[
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
#[cfg(target_os = "macos")]
const ALLOWED_READ_LITERALS: &[&str] = &[
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
#[cfg(target_os = "macos")]
const ALLOWED_RUNTIME_SUBPATHS: &[&str] = &[
    "/Applications/Xcode.app/Contents/Developer",
    "/etc/ssl",
    "/Library/Developer/CommandLineTools",
    "/System/Volumes/Data/Applications/Xcode.app/Contents/Developer",
    "/System/Volumes/Data/Library/Developer/CommandLineTools",
    "/System/Volumes/Data/private/var/select",
    "/private/var/select",
    "/private/etc/ssl",
    "/var/select",
    "/var/db/timezone",
];

/// Native local execution rooted at Tidebreak's private per-chat scratch.
pub struct LocalExecutionProvider {
    scratch_root: PathBuf,
    timeout: Duration,
    document_scripts_dir: Option<PathBuf>,
    shared_package_cache: Option<PathBuf>,
    python_runtime_dirs: Vec<PathBuf>,
    managed_node_dir: Option<PathBuf>,
    network_policy: NetworkPolicy,
}

/// Host paths resolved for one execution: the model-visible workspace and cwd,
/// the per-chat non-model-visible home for `HOME`/`TMPDIR`, and receipt storage.
struct ResolvedExecutionPaths {
    workspace: PathBuf,
    cwd: PathBuf,
    env_home: PathBuf,
    receipts: PathBuf,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct CanonicalExecFolderGrant {
    path: PathBuf,
    access: ExecFolderAccess,
    overlay: Option<PathBuf>,
}

impl LocalExecutionProvider {
    pub fn new(scratch_root: impl Into<PathBuf>, timeout: Duration) -> Result<Self, ExecError> {
        if timeout.is_zero() {
            return Err(ExecError::InvalidRequest(
                "execution timeout must be positive".into(),
            ));
        }
        Ok(Self {
            scratch_root: scratch_root.into(),
            timeout,
            document_scripts_dir: None,
            shared_package_cache: None,
            python_runtime_dirs: Vec::new(),
            managed_node_dir: None,
            network_policy: NetworkPolicy::Off,
        })
    }

    /// Apply the conversation's provider-neutral network policy.
    #[must_use]
    pub fn with_network_policy(mut self, policy: NetworkPolicy) -> Self {
        self.network_policy = policy;
        self
    }

    /// Expose the trusted bundled document helpers as a read-only subtree.
    #[must_use]
    pub fn with_document_scripts(mut self, directory: Option<PathBuf>) -> Self {
        self.document_scripts_dir = directory;
        self
    }

    /// Expose the host-verified shared package cache's wheels directory as a
    /// read-only subtree. The profile never lists it as writable, so no
    /// conversation can plant an artifact another conversation would consume.
    #[must_use]
    pub fn with_shared_package_cache(mut self, directory: Option<PathBuf>) -> Self {
        self.shared_package_cache = directory;
        self
    }

    /// Expose a supported host Python runtime and its linked libraries.
    ///
    /// Its subtree becomes readable and `<directory>/bin` precedes the system
    /// paths, so `python3` in the sandbox matches the interpreter that acquired
    /// the shared wheel cache.
    #[must_use]
    pub fn with_python_runtime(
        mut self,
        directory: Option<PathBuf>,
        read_only_paths: Vec<PathBuf>,
    ) -> Self {
        self.python_runtime_dirs = directory.into_iter().chain(read_only_paths).collect();
        self
    }

    /// Expose a host-managed Node runtime rooted at `directory`: its subtree
    /// becomes readable and `<directory>/bin` is prepended to the sandbox's
    /// `PATH`.
    ///
    /// The sandbox already allows `process-exec`, so a read allowance is the
    /// whole gate — without one, `node` is a path the process cannot open. The
    /// runtime is a pinned install the host verified and keeps writing itself;
    /// like the package cache, it is never listed as writable, so a sandbox
    /// consumes the interpreter it is given and cannot replace it for the next
    /// conversation.
    #[must_use]
    pub fn with_managed_node(mut self, directory: Option<PathBuf>) -> Self {
        self.managed_node_dir = directory;
        self
    }

    /// Structured availability of the native local sandbox on this host.
    ///
    /// `Ok(())` means the mandatory confinement primitive exists. The error is
    /// a stable reason code, so a caller can report *why* local execution is
    /// impossible without re-deriving the platform rules itself.
    pub fn availability() -> Result<(), ExecUnavailableReason> {
        if !cfg!(target_os = "macos") {
            return Err(ExecUnavailableReason::UnsupportedPlatform);
        }
        if !Path::new(SANDBOX_EXEC).is_file() {
            return Err(ExecUnavailableReason::MissingSandboxBinary);
        }
        Ok(())
    }

    /// Host knowledge about the local adapter's egress enforcement: Seatbelt
    /// exposes only the execution-scoped broker port and the broker applies
    /// the destination policy outside the workload, with no bypass exception.
    #[must_use]
    pub fn egress_enforcement() -> tidebreak_egress::EgressEnforcement {
        tidebreak_egress::EgressEnforcement::external(Vec::new())
    }

    fn resolve_paths(&self, request: &ExecRequest) -> Result<ResolvedExecutionPaths, ExecError> {
        let root = fs::canonicalize(&self.scratch_root)
            .map_err(|_| ExecError::Sandbox("private scratch root is unavailable".into()))?;
        let workspace_candidate = root.join(request.workspace_id.as_str());
        let metadata = fs::symlink_metadata(&workspace_candidate)
            .map_err(|_| ExecError::Sandbox("private workspace is unavailable".into()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ExecError::Sandbox(
                "private workspace is not a regular directory".into(),
            ));
        }
        let workspace = fs::canonicalize(workspace_candidate)
            .map_err(|_| ExecError::Sandbox("private workspace is unavailable".into()))?;
        if !workspace.starts_with(&root) {
            return Err(ExecError::Sandbox(
                "private workspace escaped its root".into(),
            ));
        }
        let cwd = fs::canonicalize(workspace.join(&request.cwd))
            .map_err(|_| ExecError::Sandbox("working directory is unavailable".into()))?;
        if !cwd.starts_with(&workspace) || !cwd.is_dir() {
            return Err(ExecError::Sandbox(
                "working directory escaped the private workspace".into(),
            ));
        }
        let receipts = root.join(RECEIPT_DIR);
        secure_dir(&receipts)?;
        // A per-chat home for the sandboxed process's HOME/TMPDIR, deliberately
        // outside the model-visible workspace: interpreters drop caches under
        // $HOME (system Python writes ~/Library/Caches/com.apple.python), and
        // anything landing inside the workspace would surface as chat files and
        // be mirrored into remote sandboxes. Like receipts, it is a dotted
        // sibling of the workspace directories at the scratch root, so file
        // tools and provider sync never see it.
        let env_home = root.join(ENV_HOME_DIR).join(request.workspace_id.as_str());
        secure_dir(&env_home)?;
        Ok(ResolvedExecutionPaths {
            workspace,
            cwd,
            env_home,
            receipts,
        })
    }

    fn ensured_root(&self) -> Result<PathBuf, ExecError> {
        fs::create_dir_all(&self.scratch_root)
            .map_err(|_| ExecError::Sandbox("private scratch root is unavailable".into()))?;
        fs::canonicalize(&self.scratch_root)
            .map_err(|_| ExecError::Sandbox("private scratch root is unavailable".into()))
    }

    fn existing_root(&self) -> Result<Option<PathBuf>, ExecError> {
        match fs::canonicalize(&self.scratch_root) {
            Ok(root) => Ok(Some(root)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(ExecError::Sandbox(
                "private scratch root is unavailable".into(),
            )),
        }
    }

    /// Open the host-owned scratch `root` as a pinned descriptor. The root is
    /// canonicalized by the caller and is not sandbox-writable, so ambient
    /// resolution of it carries no containment question.
    async fn root_dir(root: &Path) -> Result<ScratchDir, ExecError> {
        resolve_scratch_directory(root, "", false)
            .await
            .ok_or_else(|| ExecError::Sandbox("private scratch root is unavailable".into()))
    }

    /// Open the workspace directory one level under `root` as a pinned
    /// descriptor, creating it when `create` is set. A symlink or
    /// non-directory at the workspace name refuses rather than being adopted
    /// or followed.
    async fn workspace_under(
        root: &ScratchDir,
        workspace: &ExecutionWorkspaceId,
        create: bool,
    ) -> Result<Option<ScratchDir>, ExecError> {
        let name = workspace.as_str();
        let opened = if create {
            root.create_dir(name).await
        } else {
            root.open_dir(name).await
        };
        match opened {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => {
                // The no-follow open is what refused; the stats only label the
                // refusal and are not what enforced it.
                if root.is_symlink(name).await || root.file_stamp(name).await.is_some() {
                    return Err(ExecError::Sandbox(
                        "private workspace is not a regular directory".into(),
                    ));
                }
                Err(ExecError::Sandbox(
                    "private workspace is unavailable".into(),
                ))
            }
        }
    }

    async fn workspace_in(
        root: &Path,
        workspace: &ExecutionWorkspaceId,
        create: bool,
    ) -> Result<Option<ScratchDir>, ExecError> {
        let root = Self::root_dir(root).await?;
        Self::workspace_under(&root, workspace, create).await
    }

    async fn ensured_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<ScratchDir, ExecError> {
        let root = self.ensured_root()?;
        Self::workspace_in(&root, workspace, true)
            .await?
            .ok_or_else(|| ExecError::Sandbox("private workspace is unavailable".into()))
    }

    /// Open `path`'s parent directory inside `workspace` as a pinned
    /// descriptor, so a symlinked intermediate directory cannot escape the
    /// workspace and cannot be swapped for one after the check. The final
    /// component's own type is checked separately, against that descriptor.
    ///
    /// The walk descends one pinned child descriptor at a time and establishes
    /// containment before creating anything, so a write through a planted
    /// symlinked parent refuses without having made directories outside the
    /// workspace first. `None` means the parent did not resolve inside the
    /// workspace — a missing directory, a planted symlink, or a component that
    /// is not a directory.
    async fn resolve_parent(
        workspace: &ScratchDir,
        path: &WorkspaceFilePath,
        create_parents: bool,
    ) -> Option<ScratchDir> {
        let relative = path.as_str();
        let parent = relative
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
        let mut directory = workspace.clone();
        for component in parent.split('/').filter(|part| !part.is_empty()) {
            directory = if create_parents {
                directory.create_dir(component).await
            } else {
                directory.open_dir(component).await
            }
            .ok()?;
        }
        Some(directory)
    }
}

#[async_trait]
impl ExecProvider for LocalExecutionProvider {
    async fn execute(&self, request: ExecRequest) -> Result<ExecResponse, ExecError> {
        request.validate()?;
        if let Err(reason) = Self::availability() {
            return Err(ExecError::Unavailable(format!(
                "native local sandboxing is not available on this host: {}",
                reason.message()
            )));
        }
        let ResolvedExecutionPaths {
            workspace,
            cwd,
            env_home,
            receipts,
        } = self.resolve_paths(&request)?;
        // Folder paths are host state injected by the configured provider, not
        // tool arguments. Revalidate them for every new invocation before any
        // path becomes a Seatbelt allowance. Revocation changes the next
        // request; a process already running under an older profile retains
        // that profile only until the process exits.
        #[cfg(target_os = "macos")]
        let folder_grants = canonicalize_folder_grants(&request.folder_grants)?;
        #[cfg(not(target_os = "macos"))]
        let folder_grants = Vec::new();
        #[cfg(target_os = "macos")]
        let denied_host_path = direct_denied_host_path(
            &request,
            &workspace,
            &env_home,
            self.document_scripts_dir.as_deref(),
            self.shared_package_cache.as_deref(),
            &self.python_runtime_dirs,
            self.managed_node_dir.as_deref(),
            &folder_grants,
        );
        let fingerprint = request_fingerprint(&request)?;
        let receipt_path = receipts.join(format!("{}.json", request.execution_id.as_str()));
        match begin_execution(&receipt_path, &fingerprint)? {
            BeginExecution::Cached(response) => return Ok(response),
            BeginExecution::Started => {}
        }
        #[cfg(target_os = "macos")]
        if denied_host_path.is_some() {
            let response = ExecResponse {
                provider: ExecProviderKind::Local,
                exit_code: Some(126),
                stdout: String::new(),
                stderr: sandbox_path_denied_message(!folder_grants.is_empty()),
                timed_out: false,
                output_truncated: false,
                duration_ms: 0,
                sync_notes: Vec::new(),
                degraded: None,
            };
            finish_execution(
                &receipt_path,
                &ExecutionReceipt::Completed {
                    fingerprint,
                    response: response.clone(),
                },
            )?;
            return Ok(response);
        }
        let document_scripts_dir = self
            .document_scripts_dir
            .as_ref()
            .map(|path| {
                fs::canonicalize(path).map_err(|_| {
                    ExecError::Sandbox("bundled document helper directory is unavailable".into())
                })
            })
            .transpose()?;
        let shared_package_cache = self
            .shared_package_cache
            .as_ref()
            .map(|path| {
                fs::canonicalize(path)
                    .map_err(|_| ExecError::Sandbox("shared package cache is unavailable".into()))
            })
            .transpose()?;
        // A user-managed runtime that disappears between turns drops out like
        // the managed Node slot. The system Python remains available as a
        // fallback, but it receives no package cache when it is below 3.11.
        let python_runtime_dirs = self
            .python_runtime_dirs
            .first()
            .and_then(|prefix| fs::canonicalize(prefix).ok())
            .map(|prefix| {
                std::iter::once(prefix)
                    .chain(
                        self.python_runtime_dirs
                            .iter()
                            .skip(1)
                            .filter(|path| path.is_dir())
                            .cloned(),
                    )
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        // A runtime that has been uninstalled between turns simply drops out:
        // the profile and PATH go back to what they were rather than failing
        // the command over an absent convenience.
        let managed_node_dir = self
            .managed_node_dir
            .as_ref()
            .and_then(|path| fs::canonicalize(path).ok());
        let result = run_native(
            &request,
            &workspace,
            &cwd,
            &env_home,
            self.timeout,
            document_scripts_dir.as_deref(),
            shared_package_cache.as_deref(),
            &python_runtime_dirs,
            managed_node_dir.as_deref(),
            &folder_grants,
            &self.network_policy,
        )
        .await;
        match result {
            Ok(response) => {
                #[cfg(target_os = "macos")]
                let response = {
                    let mut response = response;
                    annotate_seatbelt_access_denial(
                        &workspace,
                        &env_home,
                        document_scripts_dir.as_deref(),
                        shared_package_cache.as_deref(),
                        &python_runtime_dirs,
                        managed_node_dir.as_deref(),
                        &folder_grants,
                        &mut response,
                    );
                    response
                };
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
    async fn create_workspace(&self, workspace: &ExecutionWorkspaceId) -> Result<(), ExecError> {
        self.ensured_workspace(workspace).await.map(|_| ())
    }

    async fn connect_workspace(&self, workspace: &ExecutionWorkspaceId) -> Result<bool, ExecError> {
        let Some(root) = self.existing_root()? else {
            return Ok(false);
        };
        Ok(Self::workspace_in(&root, workspace, false).await?.is_some())
    }

    async fn destroy_workspace(&self, workspace: &ExecutionWorkspaceId) -> Result<(), ExecError> {
        let Some(root) = self.existing_root()? else {
            return Ok(());
        };
        let root = Self::root_dir(&root).await?;
        // Drop the chat's non-model-visible env home (the sandbox process's
        // HOME/TMPDIR) alongside its workspace, so destroyed chats leave no
        // interpreter caches behind. Absence of either is not a failure.
        match root.open_dir(ENV_HOME_DIR).await {
            Ok(env_homes) => match env_homes.remove_dir_all(workspace.as_str()).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(ExecError::Sandbox(
                        "could not remove private execution storage".into(),
                    ))
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(ExecError::Sandbox(
                    "could not remove private execution storage".into(),
                ))
            }
        }
        if Self::workspace_under(&root, workspace, false)
            .await?
            .is_none()
        {
            return Ok(());
        }
        root.remove_dir_all(workspace.as_str())
            .await
            .map_err(|_| ExecError::Sandbox("could not remove private workspace".into()))
    }

    async fn put_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<(), ExecError> {
        if content.len() > MAX_WORKSPACE_FILE_BYTES {
            return Err(ExecError::WorkspaceFileTooLarge);
        }
        let workspace = self.ensured_workspace(workspace).await?;
        let parent = Self::resolve_parent(&workspace, path, true)
            .await
            .ok_or_else(|| ExecError::Sandbox("workspace directories are unavailable".into()))?;
        // The write goes to an unpredictable temp name created exclusively and
        // without following, then renamed into place — both relative to the
        // pinned parent. A process with write access to chat scratch (an
        // exec-tool command runs Seatbelt-confined to this same root) can
        // neither pre-plant a symlink at the temp path nor swap the parent
        // directory itself for one after it was resolved.
        parent
            .write_file(path.file_name(), content)
            .await
            .map_err(|_| ExecError::Sandbox("could not write the workspace file".into()))
    }

    async fn get_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
    ) -> Result<Vec<u8>, ExecError> {
        let Some(root) = self.existing_root()? else {
            return Err(ExecError::WorkspaceFileNotFound);
        };
        let Some(workspace) = Self::workspace_in(&root, workspace, false).await? else {
            return Err(ExecError::WorkspaceFileNotFound);
        };
        let Some(parent) = Self::resolve_parent(&workspace, path, false).await else {
            return Err(ExecError::WorkspaceFileNotFound);
        };
        // Open relative to the pinned parent without following the final
        // component, then judge the opened descriptor — never a path stat'd
        // separately from the open. A resolve-then-open-by-path read races a
        // writer that keeps the path a regular file at the check and swaps in a
        // symlink to a host secret before the open; containment here comes from
        // the descriptors actually used.
        let file = match parent.open_file(path.file_name()).await {
            Ok(file) => file,
            Err(error) => {
                // A planted symlink fails the no-follow open. Label it as an
                // invalid path rather than a missing file so it is not silently
                // indistinguishable from absence; the stat explains the refusal
                // and is not what enforced it.
                if parent.is_symlink(path.file_name()).await {
                    return Err(ExecError::InvalidRequest(
                        "workspace path is not a regular file".into(),
                    ));
                }
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Err(ExecError::WorkspaceFileNotFound);
                }
                return Err(ExecError::Sandbox(
                    "could not read the workspace file".into(),
                ));
            }
        };
        let metadata = file
            .metadata()
            .map_err(|_| ExecError::Sandbox("could not read the workspace file".into()))?;
        if !metadata.is_file() {
            return Err(ExecError::InvalidRequest(
                "workspace path is not a regular file".into(),
            ));
        }
        if metadata.len() > MAX_WORKSPACE_FILE_BYTES as u64 {
            return Err(ExecError::WorkspaceFileTooLarge);
        }
        let mut content = Vec::new();
        file.take(MAX_WORKSPACE_FILE_BYTES as u64 + 1)
            .read_to_end(&mut content)
            .map_err(|_| ExecError::Sandbox("could not read the workspace file".into()))?;
        if content.len() > MAX_WORKSPACE_FILE_BYTES {
            return Err(ExecError::WorkspaceFileTooLarge);
        }
        Ok(content)
    }

    async fn list_workspace_files(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: Option<&WorkspaceFilePath>,
    ) -> Result<WorkspaceListing, ExecError> {
        let workspace = self.ensured_workspace(workspace).await?;
        let base = match path {
            None => workspace,
            Some(path) => {
                // Descend one pinned descriptor at a time; the no-follow open
                // is what refuses, and a directory swapped out from under the
                // walk is one a descriptor no longer refers to.
                let components = path
                    .as_str()
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>();
                let mut base = workspace;
                for (index, component) in components.iter().enumerate() {
                    base = match base.open_dir(component).await {
                        Ok(child) => child,
                        Err(error) => {
                            // The stats below only label the refusal; they are
                            // not what enforced it.
                            if error.kind() == std::io::ErrorKind::NotFound {
                                return Err(ExecError::WorkspaceFileNotFound);
                            }
                            if base.is_symlink(component).await {
                                return Err(ExecError::Sandbox(
                                    "workspace path escaped the private workspace".into(),
                                ));
                            }
                            if index + 1 == components.len()
                                && base.file_stamp(component).await.is_some()
                            {
                                return Err(ExecError::InvalidRequest(
                                    "workspace path is not a directory".into(),
                                ));
                            }
                            return Err(ExecError::Sandbox("could not list the workspace".into()));
                        }
                    };
                }
                base
            }
        };
        let mut entries = Vec::new();
        for entry in base
            .entries()
            .await
            .map_err(|_| ExecError::Sandbox("could not list the workspace".into()))?
        {
            let (directory, size_bytes) = match entry.kind {
                ScratchEntryKind::Directory => (true, None),
                ScratchEntryKind::File => {
                    // A file whose stamp cannot be read is skipped, as one
                    // whose metadata could not be read was before.
                    let Some(stamp) = base.file_stamp(&entry.name).await else {
                        continue;
                    };
                    (false, Some(stamp.len))
                }
                ScratchEntryKind::Other => continue,
            };
            let relative = match path {
                None => entry.name,
                Some(path) => format!("{}/{}", path.as_str(), entry.name),
            };
            entries.push(WorkspaceFileEntry {
                path: relative,
                directory,
                size_bytes,
            });
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        let truncated = entries.len() > MAX_WORKSPACE_LIST_ENTRIES;
        entries.truncate(MAX_WORKSPACE_LIST_ENTRIES);
        Ok(WorkspaceListing { entries, truncated })
    }
}

fn begin_execution(path: &Path, fingerprint: &str) -> Result<BeginExecution, ExecError> {
    let receipt = ExecutionReceipt::running(fingerprint);
    let bytes = serde_json::to_vec(&receipt)
        .map_err(|_| ExecError::Sandbox("could not encode receipt".into()))?;
    begin_execution_with_persistence(path, fingerprint, |file| {
        file.write_all(&bytes).and_then(|()| file.sync_all())
    })
}

fn begin_execution_with_persistence(
    path: &Path,
    fingerprint: &str,
    persist: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
) -> Result<BeginExecution, ExecError> {
    let parent = path
        .parent()
        .ok_or_else(|| ExecError::Sandbox("execution receipt has no parent".into()))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(path) {
        Ok(mut file) => {
            if persist(&mut file).and_then(|()| sync_dir(parent)).is_err() {
                drop(file);
                discard_unstarted_receipt(path, parent)?;
                return Err(ExecError::Sandbox("could not persist receipt".into()));
            }
            Ok(BeginExecution::Started)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let receipt = read_receipt(path)?;
            receipt.replay(fingerprint, ExecError::Sandbox)
        }
        Err(_) => Err(ExecError::Sandbox(
            "could not create execution receipt".into(),
        )),
    }
}

fn discard_unstarted_receipt(path: &Path, parent: &Path) -> Result<(), ExecError> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(ExecError::Sandbox(
                "could not clean up incomplete execution receipt".into(),
            ));
        }
    }
    sync_dir(parent)
        .map_err(|_| ExecError::Sandbox("could not clean up incomplete execution receipt".into()))
}

fn read_receipt(path: &Path) -> Result<ExecutionReceipt, ExecError> {
    let file = fs::File::open(path)
        .map_err(|_| ExecError::Sandbox("could not read execution receipt".into()))?;
    if file
        .metadata()
        .map_err(|_| ExecError::Sandbox("could not inspect execution receipt".into()))?
        .len()
        > MAX_RECEIPT_BYTES
    {
        return Err(ExecError::Sandbox(
            "execution receipt exceeds its bound".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ExecError::Sandbox("could not read execution receipt".into()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ExecError::Sandbox("execution receipt is invalid".into()))
}

fn finish_execution(path: &Path, receipt: &ExecutionReceipt) -> Result<(), ExecError> {
    let parent = path
        .parent()
        .ok_or_else(|| ExecError::Sandbox("execution receipt has no parent".into()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ExecError::Sandbox("execution receipt name is invalid".into()))?;
    let temporary = parent.join(format!(".{file_name}.terminal"));
    let bytes = serde_json::to_vec(receipt)
        .map_err(|_| ExecError::Sandbox("could not encode receipt".into()))?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(ExecError::Sandbox(
            "execution receipt exceeds its bound".into(),
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .map_err(|_| ExecError::AmbiguousExecution)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| ExecError::AmbiguousExecution)?;
    fs::rename(&temporary, path).map_err(|_| ExecError::AmbiguousExecution)?;
    sync_dir(parent).map_err(|_| ExecError::AmbiguousExecution)
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn secure_dir(path: &Path) -> Result<(), ExecError> {
    fs::create_dir_all(path)
        .map_err(|_| ExecError::Sandbox("could not create private execution storage".into()))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| ExecError::Sandbox("could not inspect private execution storage".into()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ExecError::Sandbox(
            "private execution storage is not a regular directory".into(),
        ));
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| ExecError::Sandbox("could not secure private execution storage".into()))?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
async fn run_native(
    request: &ExecRequest,
    workspace: &Path,
    cwd: &Path,
    env_home: &Path,
    timeout: Duration,
    document_scripts_dir: Option<&Path>,
    shared_package_cache: Option<&Path>,
    python_runtime_dirs: &[PathBuf],
    managed_node_dir: Option<&Path>,
    folder_grants: &[CanonicalExecFolderGrant],
    network_policy: &NetworkPolicy,
) -> Result<ExecResponse, ExecError> {
    let broker = if matches!(network_policy, NetworkPolicy::Off) {
        None
    } else {
        Some(LocalEgressBroker::start(network_policy.clone()).await?)
    };
    let developer_dir = macos_developer_dir();
    let profile = macos_profile(
        workspace,
        env_home,
        developer_dir.as_deref(),
        document_scripts_dir,
        shared_package_cache,
        python_runtime_dirs,
        managed_node_dir,
        folder_grants,
        broker.as_ref().map(LocalEgressBroker::port),
    )?;
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .args(["-p", &profile, "--", &request.command])
        .args(&request.arguments)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", env_home)
        .env("TMPDIR", env_home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(broker) = broker.as_ref() {
        let proxy = broker.proxy_url();
        command
            .env("HTTP_PROXY", &proxy)
            .env("HTTPS_PROXY", &proxy)
            .env("http_proxy", &proxy)
            .env("https_proxy", &proxy)
            .env("ALL_PROXY", &proxy)
            .env("all_proxy", &proxy)
            .env("NO_PROXY", "")
            .env("no_proxy", "");
    }
    if let Some(developer_dir) = developer_dir.as_deref() {
        command.env("DEVELOPER_DIR", developer_dir);
    }
    command.env(
        "PATH",
        sandbox_path(
            developer_dir.as_deref(),
            python_runtime_dirs.first().map(PathBuf::as_path),
            managed_node_dir,
        ),
    );
    if let Some(directory) = document_scripts_dir {
        command.env("TIDEBREAK_EXEC_SCRIPTS", directory);
    }
    if let Some(directory) = shared_package_cache {
        // `PIP_FIND_LINKS` makes every pip invocation consider the verified
        // local wheels alongside (or, with `--no-index`, instead of) the
        // registry; the Tidebreak-named variable is what the operating prompt
        // steers offline installs with.
        command
            .env(crate::package_cache::PACKAGE_CACHE_ENV, directory)
            .env("PIP_FIND_LINKS", directory);
    }
    configure_unix_limits(&mut command, timeout);

    let started = Instant::now();
    let mut child = command.spawn().map_err(|_| ExecError::Spawn)?;
    let process_group = child.id().map(|id| id as i32);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ExecError::Sandbox("stdout capture is unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ExecError::Sandbox("stderr capture is unavailable".into()))?;
    let capture = Arc::new(Mutex::new(Capture::default()));
    let stdout_reader = tokio::spawn(drain_output(stdout, capture.clone(), StreamKind::Stdout));
    let stderr_reader = tokio::spawn(drain_output(stderr, capture.clone(), StreamKind::Stderr));

    let (status, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(waited) => (
            waited.map_err(|_| ExecError::Sandbox("could not wait for command".into()))?,
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
                    .map_err(|_| ExecError::Sandbox("could not stop command".into()))?;
            }
            (
                child
                    .wait()
                    .await
                    .map_err(|_| ExecError::Sandbox("could not reap command".into()))?,
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
    Ok(capture.response(ExecProviderKind::Local, started, status.code(), timed_out))
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::too_many_arguments)]
async fn run_native(
    _request: &ExecRequest,
    _workspace: &Path,
    _cwd: &Path,
    _env_home: &Path,
    _timeout: Duration,
    _document_scripts_dir: Option<&Path>,
    _shared_package_cache: Option<&Path>,
    _python_runtime_dirs: &[PathBuf],
    _managed_node_dir: Option<&Path>,
    _folder_grants: &[()],
    _network_policy: &NetworkPolicy,
) -> Result<ExecResponse, ExecError> {
    Err(ExecError::Unavailable(
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
/// The sandbox's `PATH`, pinned to directories the host controls.
///
/// Supported host runtimes go before the fixed system paths.
///
/// The directories come from host-side probes, not model input. The user's
/// shell `PATH` never reaches a sandboxed command.
fn sandbox_path(
    developer_dir: Option<&Path>,
    python_runtime_dir: Option<&Path>,
    managed_node_dir: Option<&Path>,
) -> String {
    let mut entries = Vec::new();
    if let Some(directory) = managed_node_dir {
        entries.push(format!("{}/bin", directory.display()));
    }
    if let Some(directory) = python_runtime_dir {
        entries.push(format!("{}/bin", directory.display()));
    }
    if let Some(directory) = developer_dir {
        entries.push(format!("{}/usr/bin", directory.display()));
    }
    entries.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(str::to_owned));
    entries.join(":")
}

#[cfg(target_os = "macos")]
fn direct_denied_host_path(
    request: &ExecRequest,
    workspace: &Path,
    env_home: &Path,
    document_scripts_dir: Option<&Path>,
    shared_package_cache: Option<&Path>,
    python_runtime_dirs: &[PathBuf],
    managed_node_dir: Option<&Path>,
    folder_grants: &[CanonicalExecFolderGrant],
) -> Option<PathBuf> {
    std::iter::once(request.command.as_str())
        .chain(request.arguments.iter().map(String::as_str))
        .filter_map(direct_absolute_path)
        .find(|path| {
            denied_host_path(
                path,
                workspace,
                env_home,
                document_scripts_dir,
                shared_package_cache,
                python_runtime_dirs,
                managed_node_dir,
                folder_grants,
            )
        })
        .map(Path::to_owned)
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn annotate_seatbelt_access_denial(
    workspace: &Path,
    env_home: &Path,
    document_scripts_dir: Option<&Path>,
    shared_package_cache: Option<&Path>,
    python_runtime_dirs: &[PathBuf],
    managed_node_dir: Option<&Path>,
    folder_grants: &[CanonicalExecFolderGrant],
    response: &mut ExecResponse,
) {
    if response.stdout.contains(SANDBOX_PATH_DENIED_CODE)
        || response.stderr.contains(SANDBOX_PATH_DENIED_CODE)
    {
        return;
    }

    let reports_permission_denial =
        [&response.stdout, &response.stderr]
            .into_iter()
            .any(|output| {
                output
                    .to_ascii_lowercase()
                    .contains("operation not permitted")
            });
    let reports_denied_path = [&response.stdout, &response.stderr]
        .into_iter()
        .any(|output| {
            output_contains_denied_host_path(
                output,
                workspace,
                env_home,
                document_scripts_dir,
                shared_package_cache,
                python_runtime_dirs,
                managed_node_dir,
                folder_grants,
            )
        });
    if !reports_permission_denial || !reports_denied_path {
        return;
    }

    // Seatbelt reports a denied access as EPERM, but that wording by itself is
    // ordinary child-controlled output. Require a captured absolute path that
    // resolves into a denied, non-allowed root before treating the response as
    // a sandbox denial. The path may have been expanded or constructed inside
    // the child, may be printed on either output stream, and may be caught by
    // the child before a successful exit, so normalize both channels before
    // the response is persisted or exposed to a model.
    response.stdout.clear();
    response.stderr = sandbox_path_denied_message(!folder_grants.is_empty());
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn output_contains_denied_host_path(
    output: &str,
    workspace: &Path,
    env_home: &Path,
    document_scripts_dir: Option<&Path>,
    shared_package_cache: Option<&Path>,
    python_runtime_dirs: &[PathBuf],
    managed_node_dir: Option<&Path>,
    folder_grants: &[CanonicalExecFolderGrant],
) -> bool {
    let decoded = decode_path_separators_for_classification(output);
    let output = decoded.as_str();
    let bytes = output.as_bytes();
    let mut cursor = 0;
    while let Some(relative_start) = bytes[cursor..].iter().position(|byte| *byte == b'/') {
        let start = cursor + relative_start;
        if let Some((url_end, path)) = file_url_path(output, start) {
            if denied_host_path(
                &path,
                workspace,
                env_home,
                document_scripts_dir,
                shared_package_cache,
                python_runtime_dirs,
                managed_node_dir,
                folder_grants,
            ) {
                return true;
            }
            cursor = url_end.max(start + 1);
            continue;
        }
        if let Some(url_end) = non_file_url_end(output, start) {
            cursor = url_end.max(start + 1);
            continue;
        }
        let quote = start
            .checked_sub(1)
            .filter(|index| !byte_is_escaped(bytes, *index))
            .map(|index| bytes[index])
            .filter(|byte| matches!(byte, b'\'' | b'"'));
        let end = quote
            .and_then(|quote| find_unescaped_quote(bytes, start, quote))
            .unwrap_or_else(|| {
                bytes[start..]
                    .iter()
                    .position(|byte| unquoted_path_delimiter(*byte))
                    .map_or(bytes.len(), |relative_end| start + relative_end)
            });
        let candidate = if quote.is_some() {
            &output[start..end]
        } else {
            output[start..end].trim_end_matches(unquoted_path_trailing_punctuation)
        };
        if candidate.len() > 1
            && denied_host_path(
                Path::new(candidate),
                workspace,
                env_home,
                document_scripts_dir,
                shared_package_cache,
                python_runtime_dirs,
                managed_node_dir,
                folder_grants,
            )
        {
            return true;
        }
        cursor = end.max(start + 1);
    }
    false
}

#[cfg(target_os = "macos")]
fn decode_path_separators_for_classification(output: &str) -> String {
    // Captured output is size-bounded. Decode only these adjacent separator
    // spellings in one pass; never recursively interpret the decoded text.
    let bytes = output.as_bytes();
    let mut decoded = String::with_capacity(output.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            if bytes.get(cursor + 1) == Some(&b'/') {
                decoded.push('/');
                cursor += 2;
                continue;
            }
            if bytes.get(cursor + 1) == Some(&b'u')
                && bytes
                    .get(cursor + 2..cursor + 6)
                    .is_some_and(|hex| hex.eq_ignore_ascii_case(b"002f"))
            {
                decoded.push('/');
                cursor += 6;
                continue;
            }
        }

        let character = output[cursor..]
            .chars()
            .next()
            .expect("cursor remains on a character boundary");
        decoded.push(character);
        cursor += character.len_utf8();
    }
    decoded
}

#[cfg(target_os = "macos")]
fn find_unescaped_quote(bytes: &[u8], start: usize, quote: u8) -> Option<usize> {
    (start..bytes.len()).find(|index| bytes[*index] == quote && !byte_is_escaped(bytes, *index))
}

#[cfg(target_os = "macos")]
fn byte_is_escaped(bytes: &[u8], index: usize) -> bool {
    bytes[..index]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

#[cfg(target_os = "macos")]
fn file_url_path(output: &str, slash: usize) -> Option<(usize, PathBuf)> {
    let (scheme_start, colon, token_end) = hierarchical_url_token_bounds(output, slash)?;
    if !output[scheme_start..colon].eq_ignore_ascii_case("file") {
        return None;
    }
    let parsed = url::Url::parse(&output[scheme_start..token_end]).ok()?;
    let path = parsed.to_file_path().ok()?;
    Some((token_end, path))
}

#[cfg(target_os = "macos")]
fn non_file_url_end(output: &str, slash: usize) -> Option<usize> {
    let (scheme_start, colon, token_end) = hierarchical_url_token_bounds(output, slash)?;
    let scheme = &output[scheme_start..colon];
    if scheme.eq_ignore_ascii_case("file") {
        return None;
    }

    let authority_start = slash + 2;
    let authority_end = output.as_bytes()[authority_start..token_end]
        .iter()
        .position(|byte| matches!(byte, b'/' | b'?' | b'#'))
        .map_or(token_end, |relative_end| authority_start + relative_end);
    if authority_end == authority_start {
        return None;
    }

    let parsed = url::Url::parse(&output[scheme_start..token_end]).ok()?;
    parsed.has_host().then_some(token_end)
}

#[cfg(target_os = "macos")]
fn hierarchical_url_token_bounds(output: &str, slash: usize) -> Option<(usize, usize, usize)> {
    let bytes = output.as_bytes();
    let colon = slash.checked_sub(1)?;
    if bytes.get(colon) != Some(&b':') || bytes.get(slash + 1) != Some(&b'/') {
        return None;
    }

    let mut scheme_start = colon;
    while scheme_start > 0 && url_scheme_byte(bytes[scheme_start - 1]) {
        scheme_start -= 1;
    }
    let scheme = &output[scheme_start..colon];
    if !scheme
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic())
        || (scheme_start > 0 && !url_start_boundary(bytes[scheme_start - 1]))
    {
        return None;
    }

    let token_end = bytes[slash + 2..]
        .iter()
        .position(|byte| url_delimiter(*byte))
        .map_or(bytes.len(), |relative_end| slash + 2 + relative_end);
    Some((scheme_start, colon, token_end))
}

#[cfg(target_os = "macos")]
fn url_scheme_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
}

#[cfg(target_os = "macos")]
fn url_start_boundary(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || byte.is_ascii_control()
        || matches!(
            byte,
            b'\'' | b'"' | b'=' | b':' | b'(' | b'[' | b'{' | b'<' | b'>' | b',' | b';'
        )
}

#[cfg(target_os = "macos")]
fn url_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || byte.is_ascii_control()
        || matches!(byte, b'\'' | b'"' | b'<' | b'>')
}

#[cfg(target_os = "macos")]
fn unquoted_path_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || byte.is_ascii_control()
        || matches!(byte, b'\'' | b'"' | b'<' | b'>')
}

#[cfg(target_os = "macos")]
fn unquoted_path_trailing_punctuation(character: char) -> bool {
    matches!(
        character,
        ':' | ',' | ';' | '.' | '!' | '?' | ')' | ']' | '}'
    )
}

#[cfg(target_os = "macos")]
fn direct_absolute_path(value: &str) -> Option<&Path> {
    let value = if value.starts_with('-') {
        value.split_once('=').map_or(value, |(_, value)| value)
    } else {
        value
    }
    .trim_matches(|character: char| matches!(character, '\'' | '"'));
    Path::new(value).is_absolute().then(|| Path::new(value))
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn denied_host_path(
    path: &Path,
    workspace: &Path,
    env_home: &Path,
    document_scripts_dir: Option<&Path>,
    shared_package_cache: Option<&Path>,
    python_runtime_dirs: &[PathBuf],
    managed_node_dir: Option<&Path>,
    folder_grants: &[CanonicalExecFolderGrant],
) -> bool {
    let resolved = resolve_existing_path_prefix(path);
    let denied = DENIED_READ_ROOTS
        .iter()
        .any(|root| resolved.starts_with(resolve_existing_path_prefix(Path::new(root))));
    if !denied {
        return false;
    }
    let explicitly_allowed = ALLOWED_READ_LITERALS
        .iter()
        .any(|allowed| resolved == resolve_existing_path_prefix(Path::new(allowed)))
        || ALLOWED_RUNTIME_SUBPATHS
            .iter()
            .any(|allowed| resolved.starts_with(resolve_existing_path_prefix(Path::new(allowed))))
        || [
            Some(workspace),
            Some(env_home),
            document_scripts_dir,
            shared_package_cache,
        ]
        .into_iter()
        .flatten()
        .chain(python_runtime_dirs.iter().map(PathBuf::as_path))
        .chain(managed_node_dir)
        .any(|allowed| resolved.starts_with(resolve_existing_path_prefix(allowed)))
        || folder_grants.iter().any(|grant| {
            resolved.starts_with(&grant.path)
                || grant
                    .overlay
                    .as_deref()
                    .is_some_and(|overlay| resolved.starts_with(overlay))
        });
    !explicitly_allowed
}

#[cfg(target_os = "macos")]
fn resolve_existing_path_prefix(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        let Ok(canonical) = fs::canonicalize(ancestor) else {
            continue;
        };
        let suffix = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
        return canonical.join(suffix);
    }
    path.to_owned()
}

#[cfg(any(target_os = "macos", test))]
fn sandbox_path_denied_message(has_connected_folders: bool) -> String {
    let connected = if has_connected_folders {
        "connected folders are available"
    } else {
        "no connected folders are currently available"
    };
    format!(
        "{SANDBOX_PATH_DENIED_CODE}: the requested path is outside the local execution sandbox; this is a path/capability error, not a safety refusal; available capabilities: the private chat workspace (use paths relative to '.') and {connected}; recovery: attach or copy the file into the chat workspace, or connect its containing folder, then retry with a workspace-relative or connected-folder path; if you cannot recover, tell the user what access is missing"
    )
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn macos_profile(
    workspace: &Path,
    env_home: &Path,
    developer_dir: Option<&Path>,
    document_scripts_dir: Option<&Path>,
    shared_package_cache: Option<&Path>,
    python_runtime_dirs: &[PathBuf],
    managed_node_dir: Option<&Path>,
    folder_grants: &[CanonicalExecFolderGrant],
    broker_port: Option<u16>,
) -> Result<String, ExecError> {
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
    let denied = DENIED_READ_ROOTS
        .iter()
        .map(|path| sbpl_subpath(path))
        .collect::<Vec<_>>()
        .join("\n  ");
    let literals = ALLOWED_READ_LITERALS
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
    let document_scripts = document_scripts_dir
        .map(sandbox_subpath)
        .transpose()?
        .unwrap_or_default();
    // The shared package cache joins the read-only allowances and is
    // deliberately absent from every write clause below: sandboxes consume
    // verified artifacts, only the host's trusted acquisition writes them.
    let package_cache = shared_package_cache
        .map(sandbox_subpath)
        .transpose()?
        .unwrap_or_default();
    // The selected Python prefix and linked library directories are read-only.
    // Reject any path broad enough to reopen the workspace or private HOME.
    if python_runtime_dirs.iter().any(|runtime| {
        runtime.parent().is_none()
            || workspace.starts_with(runtime)
            || env_home.starts_with(runtime)
    }) {
        return Err(ExecError::Sandbox(
            "the selected Python runtime is too broad for the local sandbox".into(),
        ));
    }
    let python_runtime = python_runtime_dirs
        .iter()
        .map(|path| sandbox_subpath(path))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n  ");
    // The managed Node runtime joins the same read-only allowances, and for
    // the same reason: `(allow process-exec)` above is unconditional, so being
    // able to *open* the interpreter is the entire gate on running it. The
    // host installed and verified this exact runtime and keeps sole write
    // access to it, so a sandbox can run `node` without any conversation being
    // able to swap out what the next one runs.
    let managed_node = managed_node_dir
        .map(sandbox_subpath)
        .transpose()?
        .unwrap_or_default();
    // `folder_grants` is the canonical, host-resolved list prepared above.
    // Command, arguments, cwd, and every other model-authored field are
    // deliberately absent from these profile clauses.
    //
    // A staged grant keeps its read allowance at the folder's own path and
    // moves its write allowance to the overlay. That pairing is the whole
    // containment story for staging: the only writable location is the copy,
    // so a command cannot reach the user's files even by naming them exactly.
    let granted_reads = folder_grants
        .iter()
        .flat_map(|grant| std::iter::once(&grant.path).chain(grant.overlay.as_ref()))
        .map(|path| sandbox_subpath(path))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n  ");
    let granted_writes = folder_grants
        .iter()
        .filter(|grant| grant.access == ExecFolderAccess::ReadWrite)
        .map(|grant| sandbox_subpath(grant.overlay.as_ref().unwrap_or(&grant.path)))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n  ");
    // Runtimes and package managers commonly walk toward the filesystem root
    // with metadata-only probes while resolving symlinks, prefixes, and
    // node_modules. Their explicitly allowed subtree is not enough for that
    // walk when a denied ancestor such as `/Users` remains opaque. Name only
    // the ancestors of paths the sandbox already receives through cwd, HOME,
    // PATH, or an explicit grant; their contents stay unreadable.
    let dynamic_metadata = sandbox_metadata_ancestors(
        [
            Some(workspace),
            Some(env_home),
            developer_dir,
            document_scripts_dir,
            shared_package_cache,
        ]
        .into_iter()
        .flatten()
        .chain(python_runtime_dirs.iter().map(PathBuf::as_path))
        .chain(managed_node_dir)
        .chain(folder_grants.iter().flat_map(|grant| {
            std::iter::once(grant.path.as_path()).chain(grant.overlay.as_deref())
        })),
    )?;
    let workspace = sandbox_subpath(workspace)?;
    // The per-chat env home backs the sandboxed process's HOME/TMPDIR. It is
    // writable like the workspace but lives outside the model-visible tree, so
    // interpreter caches never surface as chat files.
    let env_home = sandbox_subpath(env_home)?;
    let network = broker_port
        .map(|port| format!("(allow network-outbound (remote tcp \"localhost:{port}\"))"))
        .unwrap_or_default();
    Ok(format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow sysctl-read)\n\
         {network}\n\
         (allow file-read*)\n\
         (deny file-read*\n  {denied})\n\
         (allow file-read-metadata\n  {runtime_metadata}\n  {dynamic_metadata})\n\
         (allow file-read*\n  {literals}\n  {runtimes}\n  {selected_runtime}\n  {document_scripts}\n  {package_cache}\n  {python_runtime}\n  {managed_node}\n  {granted_reads}\n  {workspace}\n  {env_home})\n\
         (allow file-write*\n  {granted_writes}\n  {workspace}\n  {env_home}\n  (literal \"/dev/null\"))\n"
    ))
}

#[cfg(target_os = "macos")]
fn sandbox_metadata_ancestors<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
) -> Result<String, ExecError> {
    let mut ancestors = paths
        .into_iter()
        .flat_map(|path| path.ancestors().skip(1))
        .collect::<Vec<_>>();
    ancestors.sort_unstable();
    ancestors.dedup();
    ancestors
        .into_iter()
        .map(sandbox_literal)
        .collect::<Result<Vec<_>, _>>()
        .map(|entries| entries.join("\n  "))
}

#[cfg(target_os = "macos")]
fn canonicalize_folder_grants(
    grants: &[ExecFolderGrant],
) -> Result<Vec<CanonicalExecFolderGrant>, ExecError> {
    let mut canonical = Vec::<CanonicalExecFolderGrant>::new();
    for grant in grants {
        let path = canonicalize_grant_directory(&grant.path)?;
        let overlay = grant
            .overlay
            .as_deref()
            .map(canonicalize_grant_directory)
            .transpose()?;
        if let Some(existing) = canonical.iter_mut().find(|existing| existing.path == path) {
            if grant.access == ExecFolderAccess::ReadWrite {
                if existing.access == ExecFolderAccess::ReadWrite {
                    // Two write grants for one folder: staging holds only if
                    // both are staged, because one unstaged grant would make
                    // the folder directly writable and the other's overlay
                    // would be decorative.
                    existing.overlay = existing.overlay.take().and(overlay);
                } else {
                    existing.access = ExecFolderAccess::ReadWrite;
                    existing.overlay = overlay;
                }
            }
            continue;
        }
        canonical.push(CanonicalExecFolderGrant {
            path,
            access: grant.access,
            overlay,
        });
    }
    Ok(canonical)
}

#[cfg(target_os = "macos")]
fn canonicalize_grant_directory(path: &Path) -> Result<PathBuf, ExecError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| ExecError::Sandbox("granted folder is unavailable".into()))?;
    if metadata.file_type().is_symlink() {
        return Err(ExecError::Sandbox(
            "granted folder must not be a symbolic link".into(),
        ));
    }
    if !metadata.is_dir() {
        return Err(ExecError::Sandbox(
            "granted folder is not a directory".into(),
        ));
    }
    fs::canonicalize(path).map_err(|_| ExecError::Sandbox("granted folder is unavailable".into()))
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
    crate::sbpl::literal_str(path)
}

#[cfg(target_os = "macos")]
fn sbpl_subpath(path: &str) -> String {
    crate::sbpl::subpath_str(path)
}

#[cfg(target_os = "macos")]
fn sandbox_subpath(path: &Path) -> Result<String, ExecError> {
    crate::sbpl::subpath(path).map_err(|error| ExecError::Sandbox(error.to_string()))
}

#[cfg(target_os = "macos")]
fn sandbox_literal(path: &Path) -> Result<String, ExecError> {
    crate::sbpl::literal(path).map_err(|error| ExecError::Sandbox(error.to_string()))
}

#[cfg(test)]
mod tests;
