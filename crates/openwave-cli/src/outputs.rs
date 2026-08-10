//! `openwave output …` and `openwave attach …` — conversation outputs and file
//! attachment without a desktop.
//!
//! Both are thin clients of the server's HTTP surface (decision record 5): the
//! catalog, previews, version history, and revision bytes come from the output
//! routes, and an attachment goes through the same document-ingest and image
//! routes the desktop's picker feeds. The one thing that happens here rather
//! than on the server is writing exported bytes to a path, because only the
//! caller knows where the file should land.

use std::path::{Path, PathBuf};

use openwave_core::{AgentError, ChatId, OutputId, OutputRevisionId, Result};

use crate::api::client::Client;
use crate::connect::{Server, Session};

/// What `openwave output` was asked to do.
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

pub async fn run(command: Command, server: Server) -> Result<()> {
    let session = Session::open(&server).await?;
    execute(session.client(), command).await
}

/// Make the calls and render them. Split from [`run`] the way [`crate::setup`]
/// splits its own, so the work is reachable with a client the caller owns.
async fn execute(client: &Client, command: Command) -> Result<()> {
    match command {
        Command::List { chat } => {
            let catalog = client.list_outputs(chat).await?;
            if catalog.deliverables.is_empty() {
                eprintln!("openwave: this conversation has no outputs");
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
                eprintln!("openwave: more outputs exist than this listing carries");
            }
            Ok(())
        }
        Command::Show {
            chat,
            output,
            revision,
        } => {
            let preview = client.read_output(chat, output, revision).await?;
            if preview.content.is_empty() {
                return Err(AgentError::msg(format!(
                    "{} is {}, which has no text preview; use `openwave output export`",
                    preview.filename, preview.media_type
                )));
            }
            // Naming the revision on stderr keeps stdout the file's text
            // while still telling a driver which version it just read —
            // the id it would pass to `export --revision`.
            eprintln!(
                "openwave: {} at revision {}",
                preview.filename, preview.revision_id
            );
            print!("{}", preview.content);
            if preview.truncated {
                eprintln!(
                        "\nopenwave: preview truncated; use `openwave output export` for the whole file"
                    );
            }
            Ok(())
        }
        Command::Revisions { chat, output } => {
            let history = client.list_output_revisions(chat, output).await?;
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
            eprintln!(
                "openwave: wrote {written} bytes to {}",
                destination.display()
            );
            Ok(())
        }
    }
}

/// Write exported bytes, replacing the destination only once the whole file is
/// on disk — an interrupted export must not leave a truncated file where a
/// complete one used to be.
fn write_export(destination: &Path, bytes: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let temporary = match parent {
        Some(parent) => parent.join(format!(".openwave-export-{}.tmp", temporary_suffix())),
        None => PathBuf::from(format!(".openwave-export-{}.tmp", temporary_suffix())),
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
    let media_type = openwave_server::media_type::sniff_media_type_for_path(&bytes, &path);
    let title = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment")
        .to_owned();
    let session = Session::open(&server).await?;
    let client = session.client();
    if media_type.starts_with("image/") {
        let attachment_id = client.attach_image(chat, &media_type, bytes).await?;
        println!("{attachment_id}");
        eprintln!(
            "openwave: published {title} as an image attachment; reference it from the next turn"
        );
    } else {
        let document_id = client
            .attach_document(chat, &title, &media_type, bytes)
            .await?;
        println!("{document_id}");
        eprintln!("openwave: attached {title} as {media_type}");
    }
    Ok(())
}
