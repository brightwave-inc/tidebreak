//! Seatbelt confinement for the host-side LibreOffice conversion.
//!
//! Rendering an office file to PDF hands attacker-influenceable bytes to a
//! large C++ codebase, with no tool call and no approval in front of it: after
//! any successful exec, every `.docx`/`.pptx`/`.xlsx` the agent wrote under
//! `output/` is converted automatically. The converter therefore runs the way
//! the agent's own commands do — under `sandbox-exec`, on a profile built here
//! — so a LibreOffice memory-safety bug buys the document neither the user's
//! files nor the network.
//!
//! The profile is empirical: it was iterated against a real conversion of a
//! deck, a Writer document, and a spreadsheet with the pinned LibreOffice
//! 25.8.7, widened only where a run actually failed. What the conversion turned
//! out to need, and why each allowance is here rather than absent:
//!
//! - **A UNIX-domain socket.** LibreOffice always sets up its single-instance
//!   IPC pipe, even headless; denying `network*` outright makes `soffice` exit
//!   zero having written nothing. Only `local`/`remote unix` is allowed, so
//!   nothing reaches an IP address — the property that matters.
//! - **A writable `/tmp` entry for that pipe.** The socket path is compiled
//!   into LibreOffice as `/tmp/OSL_PIPE_…` and does not follow `TMPDIR`; it
//!   probes `/tmp` for writability first. The allowance is the directory entry
//!   plus a regex pinned to that filename prefix, not the directory's contents.
//! - **`mach-lookup`.** CoreText and friends fail in unpredictable ways without
//!   it. A curated `global-name` list did convert, but logged XPC failures and
//!   would be a per-macOS-version liability; the broad allowance is a deliberate
//!   trade of secondary hardening for a converter that works on every host.
//!
//! Reads are allow-by-default with the user's data denied, matching the exec
//! profile's shape (`openwave-code-execution`'s `macos_profile`): a converter
//! that cannot read `/Users` cannot exfiltrate documents, and enumerating
//! everything LibreOffice reads out of `/System` is not a bet worth taking.
//! Writes are the reverse — deny by default, with the throwaway workdir the
//! only place bytes can land.

use std::path::{Path, PathBuf};

use openwave_code_execution::sbpl;
use tokio::process::Command;

/// Reads that are denied outright. The user's own files, the login/keychain
/// stores, and installed applications: everything the conversion has no
/// business reading, whatever a crafted document talks it into.
const DENIED_READS: &[&str] = &[
    "/Applications",
    "/Library/Keychains",
    "/Library/Preferences",
    "/Network",
    "/System/Volumes/Data/Library/Keychains",
    "/System/Volumes/Data/Users",
    "/System/Volumes/Data/Volumes",
    "/Users",
    "/Volumes",
    "/opt",
    "/private/var/db/dslocal",
    "/private/var/root",
];

/// The per-user cache directory, denied by pattern rather than by prefix.
///
/// `/private/var/folders/<x>/<y>/` holds both `C` (that user's application
/// caches — worth denying) and `T` (the temp root, where the conversion's own
/// throwaway directory lives, and which LibreOffice must be able to read).
/// Denying the whole tree hangs the conversion, so only the cache half goes.
const DENIED_USER_CACHE: &str = r#"(regex #"^/private/var/folders/[^/]+/[^/]+/C(/|$)")"#;

/// LibreOffice installs whose bundle is readable even though the deny list
/// covers the directory it sits in. A user-installed copy lives under
/// `/Applications` or the home directory, and the managed install lives under
/// the app's data directory in `~/Library`; a launcher script on `PATH` execs
/// one of these rather than itself being the binary.
fn known_bundle_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications/LibreOffice.app")];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications/LibreOffice.app"));
    }
    roots
}

/// The confined converter invocation: `sandbox-exec` carrying the profile for
/// this conversion, with the converter resolved to the path the kernel will
/// see. The caller adds the LibreOffice arguments.
///
/// Executing the resolved path rather than the name it was found under is part
/// of the confinement, not tidiness: a launcher on `PATH` is usually a symlink
/// into a directory the profile denies, and the profile is written in terms of
/// what paths resolve to.
pub(crate) fn confined_command(soffice: &Path, workdir: &Path) -> Result<Command, String> {
    if !Path::new(sbpl::SANDBOX_EXEC).is_file() {
        return Err(
            "the macOS sandbox (sandbox-exec) is missing, so the converter cannot be confined"
                .to_owned(),
        );
    }
    let converter = canonical(soffice)?;
    let profile = profile(&converter, workdir)?;
    let mut command = Command::new(sbpl::SANDBOX_EXEC);
    command.arg("-p").arg(profile).arg("--").arg(&converter);
    Ok(command)
}

/// The Seatbelt profile for one conversion.
///
/// `converter` is the resolved converter path and `workdir` the throwaway
/// directory that is simultaneously the working directory, `HOME`, `TMPDIR`,
/// the LibreOffice profile, the input, and the output. Both are host-resolved;
/// no model-authored string reaches this function.
fn profile(converter: &Path, workdir: &Path) -> Result<String, String> {
    // Seatbelt matches resolved paths, so the workdir is canonicalized too: a
    // temp root reached through `/var` -> `/private/var` would otherwise be
    // allowed under a name the kernel never sees.
    let workdir = canonical(workdir)?;
    let mut read_roots = vec![bundle_root(converter)];
    read_roots.extend(
        known_bundle_roots()
            .into_iter()
            .filter(|root| root.is_dir())
            .filter_map(|root| std::fs::canonicalize(root).ok()),
    );
    read_roots.sort();
    read_roots.dedup();

    let denied = DENIED_READS
        .iter()
        .map(|path| sbpl::subpath_str(path))
        .chain(std::iter::once(DENIED_USER_CACHE.to_owned()))
        .collect::<Vec<_>>()
        .join("\n  ");
    let converter_reads = read_roots
        .iter()
        .map(|path| sbpl::subpath(path))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?
        .join("\n  ");
    // A bundle under a denied root is reached by path, and LibreOffice stats
    // its way up from the binary; metadata on the ancestors keeps that working
    // without opening the denied trees for reading.
    let mut ancestors = read_roots
        .iter()
        .flat_map(|root| root.ancestors().skip(1))
        .collect::<Vec<_>>();
    ancestors.sort_unstable();
    ancestors.dedup();
    let converter_metadata = ancestors
        .into_iter()
        .map(sbpl::literal)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?
        .join("\n  ");
    let workdir = sbpl::subpath(&workdir).map_err(|error| error.to_string())?;

    Ok(format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow signal (target self))\n\
         (allow sysctl-read)\n\
         (allow ipc-posix-shm)\n\
         (allow mach-lookup)\n\
         (allow network-bind (local unix))\n\
         (allow network-outbound (local unix) (remote unix))\n\
         (allow file-read*)\n\
         (deny file-read*\n  {denied})\n\
         (allow file-read-metadata\n  {converter_metadata})\n\
         (allow file-read*\n  {converter_reads}\n  {workdir})\n\
         (allow file-write*\n  {workdir}\n  \
         (literal \"/private/tmp\")\n  \
         (regex #\"^/private/tmp/OSL_PIPE_[^/]*$\")\n  \
         (literal \"/dev/null\"))\n"
    ))
}

/// The application bundle a converter binary belongs to, or its own directory
/// when the binary is not bundled. Allowing the bundle rather than the binary
/// is required: `soffice` is a launcher that loads the rest of the install.
fn bundle_root(converter: &Path) -> PathBuf {
    converter
        .ancestors()
        .find(|ancestor| ancestor.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            converter
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| converter.to_path_buf())
        })
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|error| {
        format!(
            "could not resolve {} for the sandbox: {error}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The properties the profile exists for: no IP network, no writes outside
    /// the throwaway workdir, and the user's files unreadable.
    #[test]
    fn profile_confines_reads_writes_and_network() {
        let workdir = tempfile::tempdir().expect("workdir");
        let install = tempfile::tempdir().expect("install");
        let bundle = install.path().join("LibreOffice.app/Contents/MacOS");
        std::fs::create_dir_all(&bundle).expect("bundle");
        let soffice = bundle.join("soffice");
        std::fs::write(&soffice, b"#!/bin/sh\n").expect("soffice");
        let converter = std::fs::canonicalize(&soffice).expect("canonical converter");

        let profile = profile(&converter, workdir.path()).expect("profile");

        assert!(profile.contains("(deny default)"));
        // The only network allowances are UNIX-domain; nothing reaches an IP.
        assert!(!profile.contains("(remote ip"), "{profile}");
        assert!(!profile.contains("network-outbound (local ip"), "{profile}");
        assert!(profile.contains("(allow network-outbound (local unix) (remote unix))"));
        // Reads: the user's home is denied and is not silently re-allowed.
        assert!(profile.contains("(deny file-read*"));
        assert!(profile.contains("(subpath \"/Users\")"));
        // Writes: the workdir, and nothing of the user's or the converter's.
        let write_rule = profile
            .split("(allow file-write*")
            .nth(1)
            .expect("profile has a write rule");
        let canonical_workdir = std::fs::canonicalize(workdir.path()).expect("canonical workdir");
        assert!(write_rule.contains(&sbpl::subpath(&canonical_workdir).unwrap()));
        assert!(!write_rule.contains("/Users"), "{write_rule}");
        assert!(!write_rule.contains("LibreOffice.app"), "{write_rule}");
        // The converter bundle is readable as a whole, not just its launcher.
        let canonical_bundle = std::fs::canonicalize(install.path())
            .expect("canonical install")
            .join("LibreOffice.app");
        assert!(
            profile.contains(&sbpl::subpath(&canonical_bundle).unwrap()),
            "{profile}"
        );
    }

    /// An install that is not an application bundle still gets a read root:
    /// the launcher's own directory, never the whole filesystem.
    #[test]
    fn unbundled_converter_falls_back_to_its_own_directory() {
        let dir = tempfile::tempdir().expect("dir");
        let soffice = dir.path().join("soffice");
        std::fs::write(&soffice, b"#!/bin/sh\n").expect("soffice");
        assert_eq!(bundle_root(&soffice), dir.path());
    }
}
