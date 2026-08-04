//! Built-in tools owned by `openwave-core`.
//!
//! The current tools operate only on a chat's private scratch directory. Their
//! model-facing definitions are centralized in [`definitions`], while
//! [`private_scratch`] owns the capability-confined filesystem primitives.

mod arguments;
mod create_app;
mod definitions;
mod list_dir;
pub(crate) mod private_scratch;
mod read_file;
mod write_file;

pub use create_app::CreateAppTool;
pub use list_dir::ListDir;
pub use read_file::ReadFile;
pub use write_file::WriteFile;

/// Model-facing contract for the built-in local-app publisher.
#[must_use]
pub fn create_app_tool_spec() -> crate::ToolSpec {
    definitions::create_app()
}

#[cfg(test)]
mod tests;
