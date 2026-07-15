use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use crate::error::Result;
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};

use super::arguments;
use super::definitions;
use super::private_scratch::{list_directory, relative_path};

/// `list_dir` — list the entries of a private scratch directory.
pub struct ListDir;

#[derive(Deserialize)]
struct Arguments {
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for ListDir {
    fn spec(&self) -> ToolSpec {
        definitions::list_dir()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, ctx: &ToolCtx, arguments: Value) -> Result<ToolOutput> {
        let arguments: Arguments = match arguments::parse(arguments) {
            Ok(arguments) => arguments,
            Err(output) => return Ok(output),
        };
        let relative = arguments.path.as_deref().unwrap_or(".");
        let path = match relative_path(relative) {
            Ok(path) => path,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let workspace = match ctx.workspace() {
            Ok(workspace) => workspace,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let result = tokio::task::spawn_blocking(move || list_directory(&workspace, &path)).await;
        match result {
            Ok(Ok(listing)) => Ok(ToolOutput::text(listing)),
            Ok(Err(error)) => Ok(ToolOutput::error(format!(
                "could not list {relative}: {error}"
            ))),
            Err(error) => Ok(ToolOutput::error(format!(
                "could not list {relative}: filesystem task failed: {error}"
            ))),
        }
    }
}
