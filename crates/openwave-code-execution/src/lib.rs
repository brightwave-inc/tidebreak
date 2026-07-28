//! Provider-neutral, bounded command execution for OpenWave.
//!
//! The model-facing [`ExecTool`] sends a normalized [`CodeExecutionRequest`] to
//! a host-selected [`CodeExecutionProvider`]. Requests carry a stable execution
//! identity and an opaque workspace identity, so a future managed adapter can
//! reconcile retries and map a chat to its own remote session without exposing
//! provider credentials or host paths to the model.
//!
//! [`LocalExecutionProvider`] runs directly on macOS under the native Seatbelt
//! sandbox. Managed providers run the same direct command contract in a remote
//! sandbox and share session, idempotency, and bounded-output primitives.

mod credential;
mod daytona;
mod e2b;
mod http;
mod local;
mod output;
mod receipt;
mod remote;
mod tool;
mod types;

pub use daytona::{DaytonaCredential, DaytonaExecutionProvider, DAYTONA_CREDENTIAL_KEY};
pub use e2b::{E2BCredential, E2BExecutionProvider, E2B_CREDENTIAL_KEY};
pub use local::LocalExecutionProvider;
pub use remote::RemoteSessionPool;
pub use tool::{ExecTool, EXEC_TOOL_NAME};
pub use types::{
    CodeExecutionError, CodeExecutionProvider, CodeExecutionProviderKind, CodeExecutionRequest,
    CodeExecutionResponse, ExecutionId, ExecutionWorkspaceId, MAX_ARGUMENTS, MAX_ARGUMENT_BYTES,
    MAX_CAPTURE_BYTES, MAX_COMMAND_BYTES, MAX_CWD_BYTES,
};
