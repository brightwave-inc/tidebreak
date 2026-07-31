//! Provider-neutral, bounded command execution for OpenWave.
//!
//! The model-facing [`ExecTool`] sends a normalized [`CodeExecutionRequest`] to
//! a host-selected [`CodeExecutionProvider`]. Requests carry a stable execution
//! identity and an opaque workspace identity, so a future managed adapter can
//! reconcile retries and map a chat to its own remote session without exposing
//! provider credentials or accepting model-authored host paths.
//!
//! [`LocalExecutionProvider`] runs directly on macOS under the native Seatbelt
//! sandbox. Managed providers run the same direct command contract in a remote
//! sandbox and share session, idempotency, and bounded-output primitives.

mod credential;
mod daytona;
mod e2b;
pub mod host_paths;
mod http;
mod local;
#[cfg(target_os = "macos")]
mod network;
mod output;
pub mod overlay;
mod preview;
mod receipt;
mod remote;
pub mod sync;
mod tool;
mod types;

pub use daytona::{DaytonaCredential, DaytonaExecutionProvider, DAYTONA_CREDENTIAL_KEY};
pub use e2b::{E2BCredential, E2BExecutionProvider, E2B_CREDENTIAL_KEY};
pub use host_paths::{
    resolve_scratch_directory, try_resolve_scratch_directory, FilePrecondition, FileStamp,
    ScratchDir, ScratchEntry, ScratchEntryKind, ScratchRefusal,
};
pub use local::LocalExecutionProvider;
pub use overlay::{
    materialize_file, materialized_file_matches, sweep_abandoned_overlays,
    MaterializationPrecondition, MaterializedChange, MaterializedChangeKind, NativeTrash,
    OverlayInspector, OverlayOutcome, OverlaySlot, PreparedWriteSnapshot, PriorContents,
    RejectedChange, RejectedChangeReason, StagedChange, TrashSink, WriteOverlay, WriteSnapshotSink,
    OVERLAY_DIR,
};
pub use preview::{scan_preview_directory, PreviewScan};
pub use remote::RemoteSessionPool;
pub use tool::{ExecTool, EXEC_TOOL_NAME};
pub use types::{
    CodeExecutionError, CodeExecutionProvider, CodeExecutionProviderKind, CodeExecutionRequest,
    CodeExecutionResponse, ExecFolderAccess, ExecFolderGrant, ExecutionId, ExecutionWorkspaceId,
    OutputArtifactEntry, OutputArtifactScan, OutputArtifactStatus, StagedUpload,
    WorkspaceFileEntry, WorkspaceFilePath, WorkspaceLifecycle, WorkspaceListing, MAX_ARGUMENTS,
    MAX_ARGUMENT_BYTES, MAX_CAPTURE_BYTES, MAX_COMMAND_BYTES, MAX_CWD_BYTES,
    MAX_EXEC_FOLDER_GRANTS, MAX_STAGED_PATHS, MAX_WORKSPACE_FILE_BYTES, MAX_WORKSPACE_LIST_ENTRIES,
    MAX_WORKSPACE_PATH_BYTES,
};

/// Stable workspace-relative location populated by hosts that ship the
/// document helper library.
pub const DOCUMENT_SCRIPTS_DIR: &str = ".openwave/exec-scripts";

/// Exact registry endpoints admitted by the provider-neutral package-manager
/// policy class.
pub const PACKAGE_MANAGER_DOMAINS: &[&str] = &[
    "api.nuget.org",
    "crates.io",
    "files.pythonhosted.org",
    "globalcdn.nuget.org",
    "index.crates.io",
    "plugins.gradle.org",
    "proxy.golang.org",
    "pypi.org",
    "registry.npmjs.org",
    "repo.maven.apache.org",
    "repo1.maven.org",
    "repo.packagist.org",
    "rubygems.org",
    "static.crates.io",
    "sum.golang.org",
];

/// Files copied as one indivisible helper library into an exec workspace.
pub const DOCUMENT_SCRIPT_FILES: [&str; 5] = [
    "_openwave_preview.py",
    "render_pdf.py",
    "extract_pdf_figures.py",
    "render_office.py",
    "analyze_xlsx.py",
];
