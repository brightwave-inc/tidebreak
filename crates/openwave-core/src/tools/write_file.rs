use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::error::Result;
use crate::preview::{ResultEntry, ResultEntryKind};
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};

use super::arguments;
use super::definitions;
use super::private_scratch::{file_name, relative_path, write_utf8_file};

/// `write_file` — atomically write a UTF-8 text file into private scratch.
pub struct WriteFile;

#[derive(Deserialize, JsonSchema)]
pub(super) struct Arguments {
    #[schemars(description = "Private-scratch-relative file path.")]
    path: String,
    #[schemars(description = "File contents to write.")]
    content: String,
}

#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        definitions::write_file()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Workspace
    }

    async fn execute(&self, ctx: &ToolCtx, arguments: Value) -> Result<ToolOutput> {
        let arguments: Arguments = match arguments::parse(arguments) {
            Ok(arguments) => arguments,
            Err(output) => return Ok(output),
        };
        let path = match relative_path(&arguments.path) {
            Ok(path) => path,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let workspace = match ctx.workspace() {
            Ok(workspace) => workspace,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let content = arguments.content.into_bytes();
        let content_len = content.len();
        let result =
            tokio::task::spawn_blocking(move || write_utf8_file(&workspace, &path, &content)).await;
        match result {
            Ok(Ok(())) => Ok(ToolOutput::text(format!(
                "wrote {} bytes to {}",
                content_len, arguments.path
            ))
            .with_entries(vec![ResultEntry::new(
                ResultEntryKind::File,
                file_name(&arguments.path),
            )
            .with_detail(&arguments.path)
            .with_meta(crate::preview::format_bytes(content_len as u64))])),
            Ok(Err(error)) => Ok(ToolOutput::error(format!(
                "could not write {}: {error}",
                arguments.path
            ))),
            Err(error) => Ok(ToolOutput::error(format!(
                "could not write {}: filesystem task failed: {error}",
                arguments.path
            ))),
        }
    }
}
