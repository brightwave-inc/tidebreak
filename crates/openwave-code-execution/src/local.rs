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
use openwave_core::NetworkPolicy;
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
#[cfg(target_os = "macos")]
use crate::CodeExecutionProviderKind;
use crate::{
    CodeExecutionError, CodeExecutionProvider, CodeExecutionRequest, CodeExecutionResponse,
    ExecutionWorkspaceId, WorkspaceFileEntry, WorkspaceFilePath, WorkspaceLifecycle,
    WorkspaceListing, MAX_WORKSPACE_FILE_BYTES, MAX_WORKSPACE_LIST_ENTRIES,
};
#[cfg(target_os = "macos")]
use crate::{ExecFolderAccess, ExecFolderGrant};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
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
const MAX_OPEN_FILES: u64 = 64;

/// Native local execution rooted at OpenWave's private per-chat scratch.
pub struct LocalExecutionProvider {
    scratch_root: PathBuf,
    timeout: Duration,
    document_scripts_dir: Option<PathBuf>,
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
            document_scripts_dir: None,
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

    /// Whether the mandatory native confinement primitive exists on this host.
    #[must_use]
    pub fn is_supported() -> bool {
        cfg!(target_os = "macos") && Path::new(SANDBOX_EXEC).is_file()
    }

    /// Host knowledge about the local adapter's egress enforcement: Seatbelt
    /// exposes only the execution-scoped broker port and the broker applies
    /// the destination policy outside the workload, with no bypass exception.
    #[must_use]
    pub fn egress_enforcement() -> openwave_egress::EgressEnforcement {
        openwave_egress::EgressEnforcement::external(Vec::new())
    }

    fn resolve_paths(
        &self,
        request: &CodeExecutionRequest,
    ) -> Result<ResolvedExecutionPaths, CodeExecutionError> {
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

    /// Open the host-owned scratch `root` as a pinned descriptor. The root is
    /// canonicalized by the caller and is not sandbox-writable, so ambient
    /// resolution of it carries no containment question.
    async fn root_dir(root: &Path) -> Result<ScratchDir, CodeExecutionError> {
        resolve_scratch_directory(root, "", false)
            .await
            .ok_or_else(|| {
                CodeExecutionError::Sandbox("private scratch root is unavailable".into())
            })
    }

    /// Open the workspace directory one level under `root` as a pinned
    /// descriptor, creating it when `create` is set. A symlink or
    /// non-directory at the workspace name refuses rather than being adopted
    /// or followed.
    async fn workspace_under(
        root: &ScratchDir,
        workspace: &ExecutionWorkspaceId,
        create: bool,
    ) -> Result<Option<ScratchDir>, CodeExecutionError> {
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
                    return Err(CodeExecutionError::Sandbox(
                        "private workspace is not a regular directory".into(),
                    ));
                }
                Err(CodeExecutionError::Sandbox(
                    "private workspace is unavailable".into(),
                ))
            }
        }
    }

    async fn workspace_in(
        root: &Path,
        workspace: &ExecutionWorkspaceId,
        create: bool,
    ) -> Result<Option<ScratchDir>, CodeExecutionError> {
        let root = Self::root_dir(root).await?;
        Self::workspace_under(&root, workspace, create).await
    }

    async fn ensured_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<ScratchDir, CodeExecutionError> {
        let root = self.ensured_root()?;
        Self::workspace_in(&root, workspace, true)
            .await?
            .ok_or_else(|| CodeExecutionError::Sandbox("private workspace is unavailable".into()))
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
        let fingerprint = request_fingerprint(&request)?;
        let receipt_path = receipts.join(format!("{}.json", request.execution_id.as_str()));
        match begin_execution(&receipt_path, &fingerprint)? {
            BeginExecution::Cached(response) => return Ok(response),
            BeginExecution::Started => {}
        }

        let document_scripts_dir = self
            .document_scripts_dir
            .as_ref()
            .map(|path| {
                fs::canonicalize(path).map_err(|_| {
                    CodeExecutionError::Sandbox(
                        "bundled document helper directory is unavailable".into(),
                    )
                })
            })
            .transpose()?;
        let result = run_native(
            &request,
            &workspace,
            &cwd,
            &env_home,
            self.timeout,
            document_scripts_dir.as_deref(),
            &folder_grants,
            &self.network_policy,
        )
        .await;
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
        self.ensured_workspace(workspace).await.map(|_| ())
    }

    async fn connect_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<bool, CodeExecutionError> {
        let Some(root) = self.existing_root()? else {
            return Ok(false);
        };
        Ok(Self::workspace_in(&root, workspace, false).await?.is_some())
    }

    async fn destroy_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<(), CodeExecutionError> {
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
                    return Err(CodeExecutionError::Sandbox(
                        "could not remove private execution storage".into(),
                    ))
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(CodeExecutionError::Sandbox(
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
        let workspace = self.ensured_workspace(workspace).await?;
        let parent = Self::resolve_parent(&workspace, path, true)
            .await
            .ok_or_else(|| {
                CodeExecutionError::Sandbox("workspace directories are unavailable".into())
            })?;
        // The write goes to an unpredictable temp name created exclusively and
        // without following, then renamed into place — both relative to the
        // pinned parent. A process with write access to chat scratch (an
        // exec-tool command runs Seatbelt-confined to this same root) can
        // neither pre-plant a symlink at the temp path nor swap the parent
        // directory itself for one after it was resolved.
        parent
            .write_file(path.file_name(), content)
            .await
            .map_err(|_| CodeExecutionError::Sandbox("could not write the workspace file".into()))
    }

    async fn get_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
    ) -> Result<Vec<u8>, CodeExecutionError> {
        let Some(root) = self.existing_root()? else {
            return Err(CodeExecutionError::WorkspaceFileNotFound);
        };
        let Some(workspace) = Self::workspace_in(&root, workspace, false).await? else {
            return Err(CodeExecutionError::WorkspaceFileNotFound);
        };
        let Some(parent) = Self::resolve_parent(&workspace, path, false).await else {
            return Err(CodeExecutionError::WorkspaceFileNotFound);
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
                    return Err(CodeExecutionError::InvalidRequest(
                        "workspace path is not a regular file".into(),
                    ));
                }
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Err(CodeExecutionError::WorkspaceFileNotFound);
                }
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
                                return Err(CodeExecutionError::WorkspaceFileNotFound);
                            }
                            if base.is_symlink(component).await {
                                return Err(CodeExecutionError::Sandbox(
                                    "workspace path escaped the private workspace".into(),
                                ));
                            }
                            if index + 1 == components.len()
                                && base.file_stamp(component).await.is_some()
                            {
                                return Err(CodeExecutionError::InvalidRequest(
                                    "workspace path is not a directory".into(),
                                ));
                            }
                            return Err(CodeExecutionError::Sandbox(
                                "could not list the workspace".into(),
                            ));
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
            .map_err(|_| CodeExecutionError::Sandbox("could not list the workspace".into()))?
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

fn secure_dir(path: &Path) -> Result<(), CodeExecutionError> {
    fs::create_dir_all(path).map_err(|_| {
        CodeExecutionError::Sandbox("could not create private execution storage".into())
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        CodeExecutionError::Sandbox("could not inspect private execution storage".into())
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(CodeExecutionError::Sandbox(
            "private execution storage is not a regular directory".into(),
        ));
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| {
        CodeExecutionError::Sandbox("could not secure private execution storage".into())
    })?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
async fn run_native(
    request: &CodeExecutionRequest,
    workspace: &Path,
    cwd: &Path,
    env_home: &Path,
    timeout: Duration,
    document_scripts_dir: Option<&Path>,
    folder_grants: &[CanonicalExecFolderGrant],
    network_policy: &NetworkPolicy,
) -> Result<CodeExecutionResponse, CodeExecutionError> {
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
    if let Some(directory) = document_scripts_dir {
        command.env("OPENWAVE_EXEC_SCRIPTS", directory);
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
#[allow(clippy::too_many_arguments)]
async fn run_native(
    _request: &CodeExecutionRequest,
    _workspace: &Path,
    _cwd: &Path,
    _env_home: &Path,
    _timeout: Duration,
    _document_scripts_dir: Option<&Path>,
    _folder_grants: &[()],
    _network_policy: &NetworkPolicy,
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
    env_home: &Path,
    developer_dir: Option<&Path>,
    document_scripts_dir: Option<&Path>,
    folder_grants: &[CanonicalExecFolderGrant],
    broker_port: Option<u16>,
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
    let document_scripts = document_scripts_dir
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
    let mut grant_ancestors = folder_grants
        .iter()
        .flat_map(|grant| std::iter::once(&grant.path).chain(grant.overlay.as_ref()))
        .flat_map(|path| path.ancestors().skip(1))
        .collect::<Vec<_>>();
    grant_ancestors.sort_unstable();
    grant_ancestors.dedup();
    let grant_metadata = grant_ancestors
        .into_iter()
        .map(sandbox_literal)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n  ");
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
         (allow file-read-metadata\n  {runtime_metadata}\n  {grant_metadata})\n\
         (allow file-read*\n  {literals}\n  {runtimes}\n  {selected_runtime}\n  {document_scripts}\n  {granted_reads}\n  {workspace}\n  {env_home})\n\
         (allow file-write*\n  {granted_writes}\n  {workspace}\n  {env_home}\n  (literal \"/dev/null\"))\n"
    ))
}

#[cfg(target_os = "macos")]
fn canonicalize_folder_grants(
    grants: &[ExecFolderGrant],
) -> Result<Vec<CanonicalExecFolderGrant>, CodeExecutionError> {
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
fn canonicalize_grant_directory(path: &Path) -> Result<PathBuf, CodeExecutionError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| CodeExecutionError::Sandbox("granted folder is unavailable".into()))?;
    if metadata.file_type().is_symlink() {
        return Err(CodeExecutionError::Sandbox(
            "granted folder must not be a symbolic link".into(),
        ));
    }
    if !metadata.is_dir() {
        return Err(CodeExecutionError::Sandbox(
            "granted folder is not a directory".into(),
        ));
    }
    fs::canonicalize(path)
        .map_err(|_| CodeExecutionError::Sandbox("granted folder is unavailable".into()))
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
fn sandbox_literal(path: &Path) -> Result<String, CodeExecutionError> {
    let path = path
        .to_str()
        .ok_or_else(|| CodeExecutionError::Sandbox("sandbox paths must be valid UTF-8".into()))?;
    if path.chars().any(char::is_control) {
        return Err(CodeExecutionError::Sandbox(
            "sandbox paths cannot contain control characters".into(),
        ));
    }
    Ok(sbpl_literal(path))
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

    /// The production regression this pins: a sandboxed interpreter writing
    /// under `$HOME` (system Python drops hundreds of bytecode caches there)
    /// must land outside the model-visible workspace, or the junk becomes chat
    /// files and is mirrored into remote sandboxes. `HOME` and `TMPDIR` must
    /// stay writable, just disjoint from the workspace tree.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn sandbox_home_and_tmpdir_are_writable_outside_the_workspace() {
        let (root, provider, workspace) = fixture(Duration::from_secs(3));
        let script = "printf home > \"$HOME/home-marker\" && \
                      printf tmp > \"$TMPDIR/tmp-marker\" && \
                      printf '%s' \"$HOME\"";
        let response = provider
            .execute(request(&workspace, "call-env-home", script))
            .await
            .unwrap();
        assert_eq!(response.exit_code, Some(0), "stderr: {}", response.stderr);

        let workspace_dir = fs::canonicalize(root.path().join(&workspace)).unwrap();
        let home = PathBuf::from(response.stdout.trim());
        assert!(
            !home.starts_with(&workspace_dir),
            "HOME must resolve outside the model-visible workspace, got {}",
            home.display()
        );
        assert!(home.join("home-marker").is_file());
        assert!(home.join("tmp-marker").is_file());
        assert!(!workspace_dir.join("home-marker").exists());
        assert!(!workspace_dir.join("tmp-marker").exists());

        let listed = provider
            .list_workspace_files(&ExecutionWorkspaceId::parse(&workspace).unwrap(), None)
            .await
            .unwrap();
        assert!(
            listed.entries.is_empty(),
            "env-home writes must not surface as chat files: {:?}",
            listed.entries
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn local_network_policy_exposes_only_the_broker_port() {
        use tokio::io::AsyncWriteExt as _;

        let (_root, provider, workspace) = fixture(Duration::from_secs(5));
        let provider = provider.with_network_policy(NetworkPolicy::Open);
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let direct_port = listener.local_addr().unwrap().port();
        let direct_server = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
            }
        });
        let script = format!(
            "if /usr/bin/curl --noproxy '*' -fsS --max-time 1 \
                 http://127.0.0.1:{direct_port} >/dev/null 2>&1; \
             then echo direct-open; else echo direct-blocked; fi; \
             /usr/bin/curl -sS --max-time 2 https://127.0.0.1:9 2>&1 || true"
        );
        let response = provider
            .execute(request(&workspace, "call-broker-pinhole", &script))
            .await
            .unwrap();
        direct_server.abort();

        assert_eq!(response.exit_code, Some(0), "{}", response.stderr);
        assert!(response.stdout.contains("direct-blocked"));
        assert!(
            response.stdout.contains("403"),
            "a fast broker refusal proves the exact proxy pinhole was reachable: {}",
            response.stdout
        );
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

        // A second write still succeeds alongside an unrelated planted dotfile.
        // This does not exercise the temp-name defense — a fixed `.stale`
        // suffix can never collide with the real `.workspace-put.{uuid}` name
        // by construction — it only guards against an incidental regression
        // where a stray dotfile wedged puts.
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

    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_write_creates_nothing_outside_the_workspace() {
        let outside = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let provider = LocalExecutionProvider::new(root.path(), Duration::from_secs(1)).unwrap();
        let workspace = ExecutionWorkspaceId::parse("chat-parents").unwrap();
        provider.create_workspace(&workspace).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("chat-parents/planted"))
            .unwrap();

        let write = provider
            .put_workspace_file(
                &workspace,
                &WorkspaceFilePath::parse("planted/deep/report.txt").unwrap(),
                b"payload",
            )
            .await;

        assert!(write.is_err(), "write through a planted parent must refuse");
        assert!(
            !outside.path().join("deep").exists(),
            "a refused write must not have created directories outside the workspace",
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
        let profile = macos_profile(
            Path::new("/Users/test/we\"ird\\workspace"),
            Path::new("/Users/test/env-home"),
            None,
            Some(Path::new(
                "/Applications/OpenWave.app/Contents/Resources/exec-scripts",
            )),
            &[],
            None,
        )
        .unwrap();
        assert!(profile.contains("(deny default)"));
        assert!(!profile.contains("allow network"));
        assert!(!profile.contains("mach-lookup"));
        assert!(!profile.contains("(allow process*)"));
        assert!(profile.contains("(allow process-exec)"));
        assert!(profile.contains("(allow process-fork)"));
        assert!(profile.contains("we\\\"ird\\\\workspace"));
        assert!(profile.contains("Resources/exec-scripts"));
        let write_rule = profile
            .split("(allow file-write*")
            .nth(1)
            .expect("profile has a write rule");
        assert!(!write_rule.contains("Resources/exec-scripts"));
        assert!(macos_profile(
            Path::new("/Users/test/control\nworkspace"),
            Path::new("/Users/test/env-home"),
            None,
            None,
            &[],
            None,
        )
        .is_err());

        let proxied = macos_profile(
            Path::new("/Users/test/workspace"),
            Path::new("/Users/test/env-home"),
            None,
            None,
            &[],
            Some(43127),
        )
        .unwrap();
        assert!(proxied.contains("(allow network-outbound (remote tcp \"localhost:43127\"))"));
        assert!(!proxied.contains("localhost:*"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn profile_compiles_read_and_write_folder_grants_without_widening_reads() {
        let folders = tempfile::tempdir().unwrap();
        let read_only = folders.path().join("read-only");
        let read_write = folders.path().join("read-write");
        fs::create_dir_all(&read_only).unwrap();
        fs::create_dir_all(&read_write).unwrap();
        let grants = canonicalize_folder_grants(&[
            ExecFolderGrant::new(&read_only, ExecFolderAccess::ReadOnly).unwrap(),
            ExecFolderGrant::new(&read_write, ExecFolderAccess::ReadWrite).unwrap(),
        ])
        .unwrap();
        let profile = macos_profile(
            Path::new("/Users/test/workspace"),
            Path::new("/Users/test/env-home"),
            None,
            None,
            &grants,
            None,
        )
        .unwrap();
        let canonical_read = fs::canonicalize(read_only).unwrap();
        let canonical_write = fs::canonicalize(read_write).unwrap();

        assert!(profile.contains(&sandbox_subpath(&canonical_read).unwrap()));
        assert!(profile.contains(&sandbox_subpath(&canonical_write).unwrap()));
        let write_rule = profile
            .split("(allow file-write*")
            .nth(1)
            .expect("profile has a write rule");
        assert!(!write_rule.contains(canonical_read.to_str().unwrap()));
        assert!(write_rule.contains(canonical_write.to_str().unwrap()));
        assert!(profile.contains("(deny file-read*"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn folder_grants_reject_symlinks_and_missing_roots() {
        use std::os::unix::fs::symlink;

        let folders = tempfile::tempdir().unwrap();
        let target = folders.path().join("target");
        let linked = folders.path().join("linked");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, &linked).unwrap();

        let symlink_error = canonicalize_folder_grants(&[ExecFolderGrant::new(
            &linked,
            ExecFolderAccess::ReadOnly,
        )
        .unwrap()])
        .unwrap_err();
        assert!(symlink_error.to_string().contains("symbolic link"));

        let missing_error = canonicalize_folder_grants(&[ExecFolderGrant::new(
            folders.path().join("missing"),
            ExecFolderAccess::ReadOnly,
        )
        .unwrap()])
        .unwrap_err();
        assert!(missing_error.to_string().contains("unavailable"));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn local_sandbox_reads_only_the_granted_sibling() {
        let scratch = tempfile::tempdir().unwrap();
        let workspace = "chat-grant";
        fs::create_dir(scratch.path().join(workspace)).unwrap();
        let host = tempfile::tempdir().unwrap();
        let granted = host.path().join("granted");
        let ungranted = host.path().join("ungranted");
        fs::create_dir(&granted).unwrap();
        fs::create_dir(&ungranted).unwrap();
        fs::write(granted.join("visible.txt"), "visible").unwrap();
        fs::write(ungranted.join("secret.txt"), "secret").unwrap();
        let granted_path = fs::canonicalize(&granted).unwrap();
        let ungranted_path = fs::canonicalize(&ungranted).unwrap();
        let provider = LocalExecutionProvider::new(scratch.path(), Duration::from_secs(3)).unwrap();
        let script = format!(
            "cat '{}'; if cat '{}' >/dev/null 2>&1; then printf ungranted-open; else printf ungranted-blocked; fi",
            granted_path.join("visible.txt").display(),
            ungranted_path.join("secret.txt").display()
        );
        let request = request(workspace, "call-folder-grant", &script)
            .with_folder_grants(vec![ExecFolderGrant::new(
                &granted_path,
                ExecFolderAccess::ReadOnly,
            )
            .unwrap()])
            .unwrap();

        let response = provider.execute(request).await.unwrap();
        assert_eq!(response.exit_code, Some(0), "stderr: {}", response.stderr);
        assert_eq!(response.stdout, "visibleungranted-blocked");
    }

    /// The invariant staging rests on: a staged grant is writable only at the
    /// overlay. A command that names the user's folder directly — which is the
    /// path the model has always been given — is refused rather than silently
    /// staged, so nothing reaches the real files mid-turn.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_staged_grant_is_writable_only_at_its_overlay() {
        let scratch = tempfile::tempdir().unwrap();
        let workspace = "chat-staged";
        fs::create_dir(scratch.path().join(workspace)).unwrap();
        let host = tempfile::tempdir().unwrap();
        let granted = host.path().join("granted");
        let overlay = host.path().join("overlay");
        fs::create_dir(&granted).unwrap();
        fs::create_dir(&overlay).unwrap();
        fs::write(granted.join("report.md"), "original").unwrap();
        let granted_path = fs::canonicalize(&granted).unwrap();
        let overlay_path = fs::canonicalize(&overlay).unwrap();

        let provider = LocalExecutionProvider::new(scratch.path(), Duration::from_secs(3)).unwrap();
        let script = format!(
            "cat '{}'; \
             if printf staged > '{}' 2>/dev/null; then printf ' overlay-written'; else printf ' overlay-blocked'; fi; \
             if printf direct > '{}' 2>/dev/null; then printf ' root-written'; else printf ' root-blocked'; fi",
            granted_path.join("report.md").display(),
            overlay_path.join("report.md").display(),
            granted_path.join("report.md").display(),
        );
        let request = request(workspace, "call-staged-grant", &script)
            .with_folder_grants(vec![ExecFolderGrant::new(
                &granted_path,
                ExecFolderAccess::ReadWrite,
            )
            .unwrap()
            .staged_at(&overlay_path)
            .unwrap()])
            .unwrap();

        let response = provider.execute(request).await.unwrap();
        assert_eq!(response.exit_code, Some(0), "stderr: {}", response.stderr);
        assert_eq!(
            response.stdout, "original overlay-written root-blocked",
            "stderr: {}",
            response.stderr
        );
        assert_eq!(
            fs::read_to_string(granted_path.join("report.md")).unwrap(),
            "original"
        );
        assert_eq!(
            fs::read_to_string(overlay_path.join("report.md")).unwrap(),
            "staged"
        );
    }
}
