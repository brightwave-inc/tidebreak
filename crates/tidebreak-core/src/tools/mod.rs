//! Built-in tools owned by `tidebreak-core`.
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

/// Save a host-fetched conversation artifact in private scratch without overwriting a file.
/// Paths remain confined by the same directory capability as the file tools.
pub async fn publish_conversation_artifact(
    ctx: &crate::ToolCtx,
    path: &str,
    bytes: Vec<u8>,
) -> std::result::Result<(), String> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("conversation artifact exceeds 2 MiB".into());
    }
    let path = private_scratch::relative_path(path)?;
    if !path.starts_with("conversation") || private_scratch::is_published_output_path(&path) {
        return Err("conversation artifacts must use the conversation scratch directory".into());
    }
    let workspace = ctx.workspace()?;
    tokio::task::spawn_blocking(move || {
        private_scratch::publish_immutable_file(&workspace, &path, &bytes)
    })
    .await
    .map_err(|error| error.to_string())?
}
