//! `tidebreak data …` — where a profile lives, a backup of it, and exports of
//! its conversations, without the desktop.
//!
//! Thin clients of the server's `/data` routes (decision 7), the same ones the
//! desktop's Data and privacy settings use. The one thing that happens here is
//! writing the answer to a path, because only the caller knows where the file
//! should land.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use tidebreak_core::{AgentError, Result, SessionId};
use tidebreak_server::profile_data::{
    ConversationExportFormat, ConversationExportRequest, DataCategory, DataOverview, DataStorage,
};

use crate::api::client::{Client, DownloadedFile};
use crate::connect::{Server, Session};
use crate::print::OutputFormat;

/// What `tidebreak data` was asked to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Show,
    Backup {
        destination: PathBuf,
    },
    Export {
        destination: PathBuf,
        format: ConversationExportFormat,
        chats: Vec<SessionId>,
    },
}

/// Parse the words after `tidebreak data`.
pub fn parse(args: Vec<String>) -> std::result::Result<(Command, OutputFormat), String> {
    let mut args = args.into_iter();
    let subcommand = args.next().unwrap_or_default();
    let mut positional = Vec::new();
    let mut output = OutputFormat::Text;
    let mut format = None;
    let mut chats = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output-format" => {
                let value = args.next().ok_or("--output-format requires text or json")?;
                output =
                    OutputFormat::parse(&value).ok_or("--output-format expects text or json")?;
            }
            "--format" if subcommand == "export" => {
                let value = args.next().ok_or("--format requires markdown or json")?;
                format = Some(match value.as_str() {
                    "markdown" | "md" => ConversationExportFormat::Markdown,
                    "json" => ConversationExportFormat::Json,
                    _ => return Err("--format expects markdown or json".to_owned()),
                });
            }
            "--chat" if subcommand == "export" => {
                let value = args.next().ok_or("--chat requires a chat UUID")?;
                chats.push(
                    SessionId::from_str(&value)
                        .map_err(|_| format!("--chat expects a chat UUID, not {value:?}"))?,
                );
            }
            flag if flag.starts_with("--") => {
                return Err(format!("unknown data {subcommand} argument {flag:?}"));
            }
            _ => positional.push(arg),
        }
    }
    let command = match subcommand.as_str() {
        "show" => {
            if !positional.is_empty() {
                return Err("data show does not accept arguments".to_owned());
            }
            Command::Show
        }
        "backup" => Command::Backup {
            destination: one_destination("backup", positional)?,
        },
        "export" => Command::Export {
            destination: one_destination("export", positional)?,
            format: format.unwrap_or(ConversationExportFormat::Markdown),
            chats,
        },
        _ => return Err("data accepts show, backup, or export".to_owned()),
    };
    Ok((command, output))
}

fn one_destination(verb: &str, positional: Vec<String>) -> std::result::Result<PathBuf, String> {
    let mut positional = positional.into_iter();
    let Some(destination) = positional.next() else {
        return Err(format!("data {verb} requires a destination path"));
    };
    if positional.next().is_some() {
        return Err(format!("data {verb} accepts one destination path"));
    }
    Ok(PathBuf::from(destination))
}

pub async fn run(command: Command, format: OutputFormat, server: Server) -> Result<()> {
    let session = Session::open(&server).await?;
    execute(session.client(), command, format).await
}

async fn execute(client: &Client, command: Command, format: OutputFormat) -> Result<()> {
    match command {
        Command::Show => {
            let overview = client.data_overview().await?;
            if format == OutputFormat::Json {
                return crate::json_output::print_document(&overview);
            }
            print!("{}", render_overview(&overview));
            Ok(())
        }
        Command::Backup { destination } => {
            let file = client.data_backup(&destination).await?;
            if format == OutputFormat::Json {
                return crate::json_output::print_document(&serde_json::json!({
                    "path": destination.display().to_string(),
                    "bytes": file.bytes,
                    "files": file.count,
                }));
            }
            println!(
                "Backed up {} files ({}) to {}.",
                file.count.unwrap_or_default(),
                human_bytes(file.bytes),
                destination.display()
            );
            Ok(())
        }
        Command::Export {
            destination,
            format: export_format,
            chats,
        } => {
            let request = ConversationExportRequest {
                format: export_format,
                chat_ids: (!chats.is_empty()).then_some(chats),
            };
            let file = client.data_export(&request, &destination).await?;
            if format == OutputFormat::Json {
                return crate::json_output::print_document(&serde_json::json!({
                    "path": destination.display().to_string(),
                    "bytes": file.bytes,
                    "conversations": file.count,
                }));
            }
            let count = file.count.unwrap_or_default();
            println!(
                "Exported {count} {} to {}.",
                if count == 1 {
                    "conversation"
                } else {
                    "conversations"
                },
                destination.display()
            );
            Ok(())
        }
    }
}

fn category_label(category: DataCategory) -> &'static str {
    match category {
        DataCategory::Database => "Database",
        DataCategory::Attachments => "Attachments",
        DataCategory::Outputs => "Outputs",
        DataCategory::Logs => "Logs",
        DataCategory::EngineTools => "Engine tools",
        DataCategory::Backups => "Backups",
        DataCategory::Other => "Other",
    }
}

fn render_overview(overview: &DataOverview) -> String {
    let mut out = format!("Data folder: {}\n", overview.data_dir);
    out.push_str(match overview.storage {
        DataStorage::Sqlite => "Conversations: SQLite, in the data folder\n",
        DataStorage::Postgres => "Conversations: PostgreSQL\n",
    });
    for entry in &overview.usage {
        out.push_str(&format!(
            "{:<14}{:>10}\n",
            category_label(entry.category),
            human_bytes(entry.bytes)
        ));
    }
    out.push_str(&format!(
        "{:<14}{:>10}\n",
        "Total",
        human_bytes(overview.total_bytes)
    ));
    if let Some(reason) = &overview.backup_unavailable {
        out.push_str(&format!("Backup: not available. {reason}\n"));
    }
    out
}

/// Sizes the way the desktop's settings show them: powers of 1024.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Stream a response body to `destination`, replacing it only once the whole
/// file is on disk and its length matches what the server announced. An
/// interrupted download never leaves a truncated file where a complete one
/// used to be. The file is readable by its owner only.
pub(crate) async fn write_download(
    destination: &Path,
    mut response: reqwest::Response,
) -> Result<u64> {
    let announced = response.content_length();
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let temporary = parent.join(format!(
        ".tidebreak-download-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let failed = |error: String| {
        AgentError::msg(format!(
            "could not write {}: {error}",
            destination.display()
        ))
    };
    let mut file = options
        .open(&temporary)
        .map_err(|error| failed(error.to_string()))?;
    let written = async {
        let mut written = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("the download stopped: {error}"))?
        {
            file.write_all(&chunk).map_err(|error| error.to_string())?;
            written += chunk.len() as u64;
        }
        if announced.is_some_and(|announced| announced != written) {
            return Err(format!(
                "the server announced {} bytes and sent {written}",
                announced.unwrap_or_default()
            ));
        }
        file.sync_all().map_err(|error| error.to_string())?;
        Ok(written)
    }
    .await;
    drop(file);
    match written {
        Ok(written) => {
            std::fs::rename(&temporary, destination).map_err(|error| {
                let _ = std::fs::remove_file(&temporary);
                failed(error.to_string())
            })?;
            Ok(written)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(failed(error))
        }
    }
}

impl Client {
    /// Where the profile lives and how much disk each part uses.
    pub async fn data_overview(&self) -> Result<DataOverview> {
        self.get_json(format!("{}/data", self.base_url())).await
    }

    /// Download a backup of the whole profile to `destination`.
    pub async fn data_backup(&self, destination: &Path) -> Result<DownloadedFile> {
        self.download(
            format!("{}/data/backup", self.base_url()),
            &serde_json::json!({}),
            destination,
            "x-tidebreak-backup-files",
        )
        .await
    }

    /// Download an export of the caller's conversations to `destination`.
    pub async fn data_export(
        &self,
        request: &ConversationExportRequest,
        destination: &Path,
    ) -> Result<DownloadedFile> {
        let body = serde_json::to_value(request)
            .map_err(|error| AgentError::msg(format!("could not encode the export: {error}")))?;
        self.download(
            format!("{}/data/export", self.base_url()),
            &body,
            destination,
            "x-tidebreak-conversations",
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn each_subcommand_parses_its_destination_and_flags() {
        assert_eq!(
            parse(words("show --output-format json")).unwrap(),
            (Command::Show, OutputFormat::Json)
        );
        assert_eq!(
            parse(words("backup /tmp/profile.tar.gz")).unwrap(),
            (
                Command::Backup {
                    destination: PathBuf::from("/tmp/profile.tar.gz")
                },
                OutputFormat::Text
            )
        );
        let chat = SessionId::new();
        assert_eq!(
            parse(words(&format!(
                "export /tmp/chats.json --format json --chat {chat}"
            )))
            .unwrap(),
            (
                Command::Export {
                    destination: PathBuf::from("/tmp/chats.json"),
                    format: ConversationExportFormat::Json,
                    chats: vec![chat],
                },
                OutputFormat::Text
            )
        );
        assert!(matches!(
            parse(words("export /tmp/chats.zip")).unwrap().0,
            Command::Export {
                format: ConversationExportFormat::Markdown,
                ..
            }
        ));
    }

    #[test]
    fn a_bad_invocation_says_what_is_missing() {
        assert_eq!(
            parse(words("backup")).unwrap_err(),
            "data backup requires a destination path"
        );
        assert_eq!(
            parse(words("export out.zip --format html")).unwrap_err(),
            "--format expects markdown or json"
        );
        assert_eq!(
            parse(words("backup out.tar.gz --chat 1")).unwrap_err(),
            "unknown data backup argument \"--chat\""
        );
        assert_eq!(
            parse(words("wipe")).unwrap_err(),
            "data accepts show, backup, or export"
        );
    }

    #[test]
    fn sizes_read_the_way_the_desktop_shows_them() {
        assert_eq!(human_bytes(0), "0 bytes");
        assert_eq!(human_bytes(1_023), "1023 bytes");
        assert_eq!(human_bytes(1_536), "1.5 KB");
        assert_eq!(human_bytes(12_582_912), "12.0 MB");
        assert_eq!(human_bytes(2_147_483_648), "2.0 GB");
    }
}
