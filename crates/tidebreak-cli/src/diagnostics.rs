//! Read and export the diagnostics owned by a running Tidebreak server.

use std::path::{Path, PathBuf};
use std::{fs::OpenOptions, io::Write as _};

use tidebreak_core::{AgentError, Result};

use crate::api::client::Client;
use crate::connect::{Server, Session};

pub enum Command {
    Snapshot,
    Metrics,
    Export { destination: PathBuf },
}

pub async fn run(command: Command, server: Server) -> Result<()> {
    let session = Session::open(&server).await?;
    execute(session.client(), command).await
}

async fn execute(client: &Client, command: Command) -> Result<()> {
    match command {
        Command::Snapshot => {
            let snapshot = client.diagnostics_snapshot().await?;
            let encoded = serde_json::to_string_pretty(&snapshot).map_err(|error| {
                AgentError::msg(format!("could not encode the diagnostic snapshot: {error}"))
            })?;
            println!("{encoded}");
            Ok(())
        }
        Command::Metrics => {
            let metrics = client.diagnostics_metrics().await?;
            print!("{metrics}");
            Ok(())
        }
        Command::Export { destination } => {
            let bytes = client.diagnostics_export().await?;
            let written = bytes.len();
            write_export(&destination, &bytes)?;
            eprintln!(
                "tidebreak: wrote {written} bytes to {}",
                destination.display()
            );
            Ok(())
        }
    }
}

fn write_export(destination: &Path, bytes: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let temporary = match parent {
        Some(parent) => parent.join(format!(
            ".tidebreak-diagnostics-{}-{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4()
        )),
        None => PathBuf::from(format!(
            ".tidebreak-diagnostics-{}-{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4()
        )),
    };
    let mut created = false;
    let write = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        created = true;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, destination)
    })();
    if write.is_err() && created {
        let _ = std::fs::remove_file(&temporary);
    }
    write.map_err(|error| {
        AgentError::msg(format!(
            "could not write {}: {error}",
            destination.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_writes_the_complete_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.zip");

        write_export(&path, b"new bundle").unwrap();

        assert_eq!(std::fs::read(path).unwrap(), b"new bundle");
    }

    #[cfg(unix)]
    #[test]
    fn export_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.zip");

        write_export(&path, b"private bundle").unwrap();

        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
