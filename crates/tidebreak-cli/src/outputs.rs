//! `tidebreak output …` and `tidebreak attach …` — conversation outputs and file
//! attachment without a desktop.
//!
//! Both are thin clients of the server's HTTP surface (decision record 5): the
//! catalog, previews, version history, and revision bytes come from the output
//! routes, and an attachment goes through the same document-ingest and image
//! routes the desktop's picker feeds. The one thing that happens here rather
//! than on the server is writing exported bytes to a path, because only the
//! caller knows where the file should land.

use std::path::{Path, PathBuf};

use tidebreak_core::{AgentError, ChatId, OutputId, OutputRevisionId, Result};

use crate::api::client::Client;
use crate::api::wire::{OutputPreview, OutputRevisionRow, OutputSummary};
use crate::connect::{Server, Session};
use crate::print::OutputFormat;

/// What `tidebreak output` was asked to do.
pub enum Command {
    List {
        chat: ChatId,
    },
    Show {
        chat: ChatId,
        output: OutputId,
        revision: Option<OutputRevisionId>,
    },
    Revisions {
        chat: ChatId,
        output: OutputId,
    },
    Export {
        chat: ChatId,
        output: OutputId,
        revision: Option<OutputRevisionId>,
        destination: PathBuf,
    },
}

pub async fn run(command: Command, format: OutputFormat, server: Server) -> Result<()> {
    let session = Session::open(&server).await?;
    execute(session.client(), command, format).await
}

/// Make the calls and render them. Split from [`run`] the way [`crate::setup`]
/// splits its own, so the work is reachable with a client the caller owns.
async fn execute(client: &Client, command: Command, format: OutputFormat) -> Result<()> {
    match command {
        Command::List { chat } => {
            let catalog = client.list_outputs(chat).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "deliverables": catalog
                        .deliverables
                        .iter()
                        .map(output_summary_json)
                        .collect::<Vec<_>>(),
                    "truncated": catalog.truncated,
                }));
            }
            if catalog.deliverables.is_empty() {
                eprintln!("tidebreak: this conversation has no outputs");
            }
            for output in &catalog.deliverables {
                println!(
                    "{}\t{}\t{}\t{} bytes\t{} version(s)\t{}",
                    output.output_id,
                    output.filename,
                    output.media_type,
                    output.size_bytes,
                    output.revision_count,
                    output.updated_at
                );
            }
            if catalog.truncated {
                eprintln!("tidebreak: more outputs exist than this listing carries");
            }
            Ok(())
        }
        Command::Show {
            chat,
            output,
            revision,
        } => {
            let preview = client.read_output(chat, output, revision).await?;
            if format == OutputFormat::Json {
                // One object on stdout holds both the preview text and the
                // metadata text mode splits across stderr and stdout, so a
                // driver can parse a single value without scraping banners.
                return emit(&output_preview_json(&preview));
            }
            if preview.content.is_empty() {
                return Err(AgentError::msg(format!(
                    "{} is {}, which has no text preview; use `tidebreak output export`",
                    preview.filename, preview.media_type
                )));
            }
            // Naming the revision on stderr keeps stdout the file's text
            // while still telling a driver which version it just read —
            // the id it would pass to `export --revision`.
            eprintln!(
                "tidebreak: {} at revision {}",
                preview.filename, preview.revision_id
            );
            print!("{}", preview.content);
            if preview.truncated {
                eprintln!(
                        "\ntidebreak: preview truncated; use `tidebreak output export` for the whole file"
                    );
            }
            Ok(())
        }
        Command::Revisions { chat, output } => {
            let history = client.list_output_revisions(chat, output).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "revisions": history
                        .revisions
                        .iter()
                        .map(output_revision_json)
                        .collect::<Vec<_>>(),
                }));
            }
            for revision in &history.revisions {
                println!(
                    "{}\tv{}\t{} bytes\t{}\t{}{}",
                    revision.revision_id,
                    revision.ordinal,
                    revision.size_bytes,
                    revision.produced_by,
                    revision.created_at,
                    if revision.is_current { "\tcurrent" } else { "" }
                );
            }
            Ok(())
        }
        Command::Export {
            chat,
            output,
            revision,
            destination,
        } => {
            let bytes = client.read_output_bytes(chat, output, revision).await?;
            let written = bytes.len();
            write_export(&destination, &bytes)?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "path": destination.display().to_string(),
                    "bytes": written,
                }));
            }
            eprintln!(
                "tidebreak: wrote {written} bytes to {}",
                destination.display()
            );
            Ok(())
        }
    }
}

fn output_summary_json(output: &OutputSummary) -> serde_json::Value {
    serde_json::json!({
        "outputId": output.output_id,
        "filename": output.filename,
        "mediaType": output.media_type,
        "sizeBytes": output.size_bytes,
        "revisionCount": output.revision_count,
        "updatedAt": output.updated_at,
    })
}

fn output_preview_json(preview: &OutputPreview) -> serde_json::Value {
    serde_json::json!({
        "filename": preview.filename,
        "mediaType": preview.media_type,
        "revisionId": preview.revision_id,
        "content": preview.content,
        "truncated": preview.truncated,
    })
}

fn output_revision_json(revision: &OutputRevisionRow) -> serde_json::Value {
    serde_json::json!({
        "revisionId": revision.revision_id,
        "ordinal": revision.ordinal,
        "sizeBytes": revision.size_bytes,
        "createdAt": revision.created_at,
        "producedBy": revision.produced_by,
        "isCurrent": revision.is_current,
    })
}

/// Write one JSON object on stdout, matching setup and print mode's shape so
/// the same consumer can read every CLI surface.
fn emit(value: &serde_json::Value) -> Result<()> {
    println!("{value}");
    Ok(())
}

/// Write exported bytes, replacing the destination only once the whole file is
/// on disk — an interrupted export must not leave a truncated file where a
/// complete one used to be.
fn write_export(destination: &Path, bytes: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let temporary = match parent {
        Some(parent) => parent.join(format!(".tidebreak-export-{}.tmp", temporary_suffix())),
        None => PathBuf::from(format!(".tidebreak-export-{}.tmp", temporary_suffix())),
    };
    let write =
        std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, destination));
    if write.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write.map_err(|error| {
        AgentError::msg(format!(
            "could not write {}: {error}",
            destination.display()
        ))
    })
}

/// A unique-enough temporary suffix without pulling `uuid` into this crate.
fn temporary_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

/// Attach one local file to a conversation through the ingest routes.
///
/// Images are published as image attachments and everything else is ingested as
/// a source document — the same split the desktop's single attach gesture makes,
/// decided from the bytes rather than from the file's name.
pub async fn attach(chat: ChatId, path: PathBuf, server: Server) -> Result<()> {
    let bytes = std::fs::read(&path)
        .map_err(|error| AgentError::msg(format!("could not read {}: {error}", path.display())))?;
    if bytes.is_empty() {
        return Err(AgentError::msg(format!("{} is empty", path.display())));
    }
    let media_type = tidebreak_server::media_type::sniff_media_type_for_path(&bytes, &path);
    let title = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment")
        .to_owned();
    let session = Session::open(&server).await?;
    let client = session.client();
    if media_type.starts_with("image/") {
        let attachment_id = client
            .attach_image(chat, &media_type, bytes)
            .await
            .map_err(|error| local_import_error(client, error))?;
        println!("{attachment_id}");
        eprintln!(
            "tidebreak: published {title} as an image attachment; reference it from the next turn"
        );
    } else {
        let document_id = client
            .attach_document(chat, &title, &media_type, bytes)
            .await
            .map_err(|error| local_import_error(client, error))?;
        println!("{document_id}");
        eprintln!("tidebreak: attached {title} as {media_type}");
    }
    Ok(())
}

fn local_import_error(client: &Client, error: AgentError) -> AgentError {
    if !client.has_local_import_capability() && error.to_string().contains("401 Unauthorized") {
        AgentError::msg(
            "the running desktop requires its scoped local-import capability; use --attach with \
             that desktop profile's data directory instead of --server, or restart the desktop \
             if listen.json predates this capability",
        )
    } else {
        error
    }
}
