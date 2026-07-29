use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::error::Result;
use crate::preview::{ResultEntry, ResultEntryKind};
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};

use super::arguments;
use super::definitions;
use super::private_scratch::{file_name, line_count, read_utf8_file, relative_path};

/// `read_file` — read a UTF-8 text file from private scratch.
pub struct ReadFile;

#[derive(Deserialize, JsonSchema)]
pub(super) struct Arguments {
    #[schemars(description = "Private-scratch-relative file path.")]
    path: String,
}

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        definitions::read_file()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
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
        let result = tokio::task::spawn_blocking(move || read_utf8_file(&workspace, &path)).await;
        match result {
            // The file's own text is what the model needs and is far too much
            // for a card, so the row reports the read rather than replaying it:
            // the path, and how much of it came back.
            Ok(Ok(content)) => {
                let entry = ResultEntry::new(ResultEntryKind::File, file_name(&arguments.path))
                    .with_detail(&arguments.path)
                    .with_meta(format!(
                        "{} · {}",
                        crate::preview::format_bytes(content.len() as u64),
                        line_count(&content)
                    ));
                Ok(ToolOutput::text(content).with_entries(vec![entry]))
            }
            Ok(Err(error)) => Ok(ToolOutput::error(format!(
                "could not read {}: {error}",
                arguments.path
            ))),
            Err(error) => Ok(ToolOutput::error(format!(
                "could not read {}: filesystem task failed: {error}",
                arguments.path
            ))),
        }
    }
}
