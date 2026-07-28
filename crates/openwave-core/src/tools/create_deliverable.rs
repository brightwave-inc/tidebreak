use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::deliverable::{
    output_revision_relative_path, validate_deliverable_name, CreateOutput, NewOutputRevision,
    DELIVERABLES_DIRECTORY, MAX_DELIVERABLE_BYTES, MAX_DELIVERABLE_NAME_CHARS,
};
use crate::error::Result;
use crate::id::{OutputId, OutputRevisionId};
use crate::storage::Store;
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};

use super::arguments;
use super::definitions;
use super::private_scratch::{publish_immutable_file, read_regular_file_bytes, write_utf8_file};

/// `create_deliverable` — create or update a user-visible text artifact.
pub struct CreateDeliverable {
    store: Arc<dyn Store>,
}

impl CreateDeliverable {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Arguments {
    #[schemars(
        length(min = 1, max = MAX_DELIVERABLE_NAME_CHARS),
        description = "Portable output filename ending in .md, .txt, .csv, .json, or .html."
    )]
    filename: String,
    #[schemars(
        length(min = 1, max = MAX_DELIVERABLE_BYTES),
        description = "Complete UTF-8 text contents of the output file (maximum 512 KiB)."
    )]
    content: String,
    #[schemars(
        description = "Opaque output id returned by an earlier call. Omit this to create a new output."
    )]
    output_id: Option<String>,
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
        let Some(call_id) = ctx.call_id else {
            return Ok(ToolOutput::error(
                "durable output publication requires a canonical tool-call identity",
            ));
        };
        let calls = match self.store.list_tool_calls(ctx.chat_id).await {
            Ok(calls) => calls,
            Err(error) => {
                return Ok(ToolOutput::error(format!(
                    "could not verify durable tool-call identity: {error}"
                )));
            }
        };
        let call = match calls.into_iter().find(|call| call.id == call_id) {
            Some(call) if call.name == definitions::CREATE_DELIVERABLE => call,
            Some(_) => {
                return Ok(ToolOutput::error(
                    "canonical tool-call identity is not a create_deliverable call",
                ));
            }
            None => {
                return Ok(ToolOutput::error(
                    "canonical tool-call identity is not owned by this conversation",
                ));
            }
        };
        let requested_output_id = match arguments.output_id {
            Some(raw) => match OutputId::from_str(&raw) {
                Ok(id) => Some(id),
                Err(_) => return Ok(ToolOutput::error("output_id must be an opaque UUID")),
            },
            None => None,
        };
        let output_id = requested_output_id.unwrap_or_else(|| OutputId::for_call(call_id));
        if let Some(existing) = requested_output_id {
            match self.store.get_output(existing).await? {
                Some(record) if record.chat_id != ctx.chat_id => {
                    return Ok(ToolOutput::error(
                        "output_id belongs to another conversation",
                    ));
                }
                Some(record) if record.filename != arguments.filename => {
                    return Ok(ToolOutput::error(
                        "output_id has a different filename; preserve the existing output filename when revising it",
                    ));
                }
                Some(_) => {}
                None => {
                    return Ok(ToolOutput::error(
                        "output_id does not exist in this conversation",
                    ))
                }
            }
        }
        let filename = arguments.filename;
        let source_path = Path::new(DELIVERABLES_DIRECTORY).join(&filename);
        let revision_id = OutputRevisionId::for_call(call_id);
        let relative_path = output_revision_relative_path(output_id, revision_id);
        let result =
            tokio::task::spawn_blocking(move || -> std::result::Result<Vec<u8>, String> {
                write_utf8_file(&workspace, &source_path, &content)
                    .map_err(|error| error.to_string())?;
                let published =
                    read_regular_file_bytes(&workspace, &source_path, MAX_DELIVERABLE_BYTES)?;
                if published.is_empty()
                    || published.contains(&0)
                    || std::str::from_utf8(&published).is_err()
                {
                    return Err("validated deliverable scratch file is not usable text".into());
                }
                if published != content {
                    return Err("deliverable scratch file changed during publication".into());
                }
                publish_immutable_file(&workspace, &relative_path, &published)?;
                Ok(published)
            })
            .await;
        let published = match result {
            Ok(Ok(published)) => published,
            Ok(Err(error)) => {
                return Ok(ToolOutput::error(format!(
                    "could not publish deliverable revision: {error}"
                )));
            }
            Err(error) => {
                return Ok(ToolOutput::error(format!(
                    "could not publish deliverable revision: filesystem task failed: {error}"
                )));
            }
        };
        let content_len = published.len();
        let revision = NewOutputRevision {
            id: revision_id,
            byte_len: content_len as u64,
            sha256: Sha256::digest(&published).into(),
            turn_id: Some(call.turn_id),
            citations: Vec::new(),
            created_at: call.created_at,
        };
        let record = match requested_output_id {
            Some(existing) => self.store.append_output_revision(existing, &revision).await,
            None => {
                self.store
                    .create_output(&CreateOutput {
                        id: output_id,
                        chat_id: ctx.chat_id,
                        filename,
                        revision,
                    })
                    .await
            }
        };
        match record {
            Ok(record) => Ok(ToolOutput::text(format!(
                "Published output {} revision {} ({} bytes).",
                record.id, record.current_revision, content_len
            ))
            .with_data(json!({
                "output_id": record.id,
                "revision_id": record.current_revision,
                "revision_count": record.revision_count,
            }))),
            Err(error) => Ok(ToolOutput::error(format!(
                "could not publish deliverable: {error}"
            ))),
        }
    }
}
