use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::error::Result;
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};

use super::arguments;
use super::definitions;
use super::private_scratch::{read_utf8_file, relative_path};

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
            Ok(Ok(content)) => Ok(ToolOutput::text(content)),
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
