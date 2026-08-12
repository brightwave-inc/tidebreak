use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::error::Result;
use crate::preview::{ResultEntry, ResultEntryKind};
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};

use super::arguments;
use super::definitions;
use super::private_scratch::{list_directory, relative_path};

/// `list_dir` — list the entries of a private scratch directory.
pub struct ListDir;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Arguments {
    #[serde(default)]
    #[schemars(
        with = "String",
        description = "Private-scratch-relative directory (optional)."
    )]
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
            Ok(Ok(listing)) => Ok(ToolOutput::text(&listing).with_entries(directory_entries(
                &listing,
                if relative == "." {
                    None
                } else {
                    Some(relative)
                },
            ))),
            Ok(Err(error)) => Ok(ToolOutput::error(format!(
                "could not list {relative}: {error}"
            ))),
            Err(error) => Ok(ToolOutput::error(format!(
                "could not list {relative}: filesystem task failed: {error}"
            ))),
        }
    }
}

/// The listing as rows, read back out of the text the model is given.
///
/// Derived from that one string rather than built alongside it, so the card and
/// the model can never disagree about what the directory holds. The trailing
/// slash `list_directory` appends is what marks a directory, and it is dropped
/// from the row: the icon says it better.
fn directory_entries(listing: &str, path: Option<&str>) -> Vec<ResultEntry> {
    listing
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let entry = match line.strip_suffix('/') {
                Some(name) => ResultEntry::new(ResultEntryKind::Folder, name),
                None => ResultEntry::new(ResultEntryKind::File, line),
            };
            match path {
                Some(path) => entry.with_detail(path),
                None => entry,
            }
        })
        .collect()
}
