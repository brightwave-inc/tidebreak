//! Provider-neutral, bounded command execution for OpenWave.
//!
//! The model-facing [`ExecTool`] sends a normalized [`CodeExecutionRequest`] to
//! a host-selected [`CodeExecutionProvider`]. Requests carry a stable execution
//! identity and an opaque workspace identity, so a future managed adapter can
//! reconcile retries and map a chat to its own remote session without exposing
//! provider credentials or host paths to the model.
//!
//! [`LocalExecutionProvider`] is the first adapter. It runs directly on macOS
//! under the native Seatbelt sandbox, denies network access, clears the inherited
//! environment, confines writes to one private chat scratch directory, bounds
//! time and captured output, and records a private terminal receipt. Unsupported
//! platforms fail closed rather than running a command without confinement.

mod local;
mod tool;
mod types;

pub use local::LocalExecutionProvider;
pub use tool::{ExecTool, EXEC_TOOL_NAME};
pub use types::{
    CodeExecutionError, CodeExecutionProvider, CodeExecutionProviderKind, CodeExecutionRequest,
    CodeExecutionResponse, ExecutionId, ExecutionWorkspaceId, MAX_ARGUMENTS, MAX_ARGUMENT_BYTES,
    MAX_CAPTURE_BYTES, MAX_COMMAND_BYTES, MAX_CWD_BYTES,
};
