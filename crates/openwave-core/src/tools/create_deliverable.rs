use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use crate::deliverable::{
    validate_deliverable_name, DELIVERABLES_DIRECTORY, MAX_DELIVERABLE_BYTES,
};
use crate::error::Result;
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};

use super::arguments;
use super::definitions;
use super::private_scratch::write_utf8_file;

/// `create_deliverable` — create or update a user-visible text artifact.
pub struct CreateDeliverable;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    filename: String,
    content: String,
}

#[async_trait]
impl Tool for CreateDeliverable {
    fn spec(&self) -> ToolSpec {
        definitions::create_deliverable()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Workspace
    }

    async fn execute(&self, ctx: &ToolCtx, arguments: Value) -> Result<ToolOutput> {
        let arguments: Arguments = match arguments::parse(arguments) {
            Ok(arguments) => arguments,
            Err(output) => return Ok(output),
        };
        if let Err(message) = validate_deliverable_name(&arguments.filename) {
            return Ok(ToolOutput::error(message));
        }
        if arguments.content.is_empty() {
            return Ok(ToolOutput::error("deliverable content may not be empty"));
        }
        if arguments.content.contains('\0') {
            return Ok(ToolOutput::error(
                "deliverable content may not contain null characters",
            ));
        }
        let content = arguments.content.into_bytes();
        if content.len() > MAX_DELIVERABLE_BYTES {
            return Ok(ToolOutput::error(format!(
                "deliverable is too large (maximum {MAX_DELIVERABLE_BYTES} bytes)"
            )));
        }
        let workspace = match ctx.workspace() {
            Ok(workspace) => workspace,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let filename = arguments.filename;
        let relative_path = Path::new(DELIVERABLES_DIRECTORY).join(&filename);
        let content_len = content.len();
        let result = tokio::task::spawn_blocking(move || {
            write_utf8_file(&workspace, &relative_path, &content)
        })
        .await;
        match result {
            Ok(Ok(())) => Ok(ToolOutput::text(format!(
                "Created `{filename}` ({content_len} bytes). The file is available to the user in Outputs."
            ))),
            Ok(Err(error)) => Ok(ToolOutput::error(format!(
                "could not create deliverable: {error}"
            ))),
            Err(error) => Ok(ToolOutput::error(format!(
                "could not create deliverable: filesystem task failed: {error}"
            ))),
        }
    }
}
