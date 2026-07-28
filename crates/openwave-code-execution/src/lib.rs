//! Provider-neutral, bounded command execution for OpenWave.
//!
//! The model-facing [`ExecTool`] sends a normalized [`CodeExecutionRequest`] to
//! a host-selected [`CodeExecutionProvider`]. Requests carry a stable execution
//! identity and an opaque workspace identity, so a future managed adapter can
//! reconcile retries and map a chat to its own remote session without exposing
//! provider credentials or host paths to the model.
//!
//! [`LocalExecutionProvider`] runs directly on macOS under the native Seatbelt
//! sandbox. [`E2BExecutionProvider`] runs the same direct command contract in a
//! managed E2B sandbox and reuses one live remote workspace per chat.

mod e2b;
mod local;
mod tool;
mod types;

pub use e2b::{E2BCredential, E2BExecutionProvider, E2BSessionPool, E2B_CREDENTIAL_KEY};
pub use local::LocalExecutionProvider;
pub use tool::{ExecTool, EXEC_TOOL_NAME};
pub use types::{
    CodeExecutionError, CodeExecutionProvider, CodeExecutionProviderKind, CodeExecutionRequest,
    CodeExecutionResponse, ExecutionId, ExecutionWorkspaceId, MAX_ARGUMENTS, MAX_ARGUMENT_BYTES,
    MAX_CAPTURE_BYTES, MAX_COMMAND_BYTES, MAX_CWD_BYTES,
};
