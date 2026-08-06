use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

use crate::error::Result;
use crate::preview::{ResultEntry, ResultEntryKind};
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolSpec};

use super::arguments;
use super::definitions;
use super::private_scratch::{
    file_name, is_published_output_path, journal_path, prior_contents, relative_path,
    write_utf8_file,
};

/// `write_file` — atomically write a UTF-8 text file into private scratch.
pub struct WriteFile;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
        // Publishing an output is an exec-scan responsibility, and the scan
        // attributes every revision to the call it ran for. Writing here
        // directly would publish nothing now and hand the bytes to whichever
        // later exec call happened to run next, which would then be credited
        // with the revision. Refuse and say where outputs come from.
        if is_published_output_path(&path) {
            return Ok(ToolOutput::failed(
                ToolErrorCategory::InvalidArguments,
                format!(
                    "{}/ is reserved for published outputs and cannot be written with write_file: \
                     {}. Produce user-visible files from an exec command that saves them into {}/ \
                     — those are published as durable, versioned outputs. Use another scratch path \
                     for intermediate text.",
                    crate::EXEC_OUTPUT_DIRECTORY,
                    arguments.path,
                    crate::EXEC_OUTPUT_DIRECTORY
                ),
            ));
        }
        let workspace = match ctx.workspace() {
            Ok(workspace) => workspace,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let content = Arc::new(arguments.content.into_bytes());
        let content_len = content.len();
        // What this call is about to destroy, kept before it stops existing.
        // Only an overwrite has anything to retain, and only a runtime that
        // offers undo is worth reading for.
        let journal = ctx.scratch_journal().cloned();
        let prior = match (&journal, journal_path(&path)) {
            (Some(_), Some(journal_path)) => {
                let workspace = Arc::clone(&workspace);
                let path = path.clone();
                tokio::task::spawn_blocking(move || prior_contents(&workspace, &path))
                    .await
                    .ok()
                    .flatten()
                    .map(|prior| (journal_path, prior))
            }
            _ => None,
        };
        let result = {
            let content = Arc::clone(&content);
            tokio::task::spawn_blocking(move || write_utf8_file(&workspace, &path, &content)).await
        };
        match result {
            Ok(Ok(())) => {
                if let (Some(journal), Some((journal_path, prior))) = (journal, prior) {
                    journal
                        .record_overwrite(&journal_path, prior, &content)
                        .await;
                }
                Ok(
                    ToolOutput::text(format!("wrote {} bytes to {}", content_len, arguments.path))
                        .with_entries(vec![ResultEntry::new(
                            ResultEntryKind::File,
                            file_name(&arguments.path),
                        )
                        .with_detail(&arguments.path)
                        .with_meta(crate::preview::format_bytes(content_len as u64))]),
                )
            }
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
