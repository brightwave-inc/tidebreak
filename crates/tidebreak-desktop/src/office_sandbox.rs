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
//! What a crafted document that takes over the converter could use to get out
//! is denied, and a real conversion needs none of it:
//!
//! - **Every program outside the converter's bundle.** `open`, `osascript`,
//!   and the rest of the system's tools cannot start, so the converter cannot
//!   hand a file it wrote to something that runs it.
//! - **The Mach services that lead out**, listed after the broad allowance
//!   because Seatbelt applies the last rule that matches: LaunchServices'
//!   brokers for opening files and URLs and for changing which app handles
//!   what; Apple Events, which drive other apps; the pasteboard, which holds
//!   whatever the user last copied; and the keychain, security agents, and
//!   authorization services, which hold secrets and can put an admin prompt
//!   in front of the user.
//! - **Opening anything through LaunchServices.** Its launch daemon,
//!   `com.apple.coreservices.launchservicesd`, has to stay reachable:
//!   LibreOffice creates an `NSApplication` even when headless, and AppKit
//!   aborts at startup if it cannot check in. LaunchServices still refuses to
//!   open files or apps for this sandbox, because the profile never allows
//!   `lsopen` and LaunchServices enforces that check outside the converter's
//!   process, where code in the converter cannot skip it. Never add
//!   `(allow lsopen)`.
//!
//! Reads are allow-by-default with the user's data denied, matching the exec
//! profile's shape (`tidebreak-code-execution`'s `macos_profile`): a converter
//! that cannot read `/Users` cannot exfiltrate documents, and enumerating
//! everything LibreOffice reads out of `/System` is not a bet worth taking.
//! Writes are the reverse — deny by default, with the throwaway workdir the
//! only place bytes can land.

use std::path::{Path, PathBuf};

use tidebreak_code_execution::sbpl;
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

/// Mach services the converter may not look up, although `mach-lookup` is
/// otherwise open. The profile lists these after the broad allowance, because
/// Seatbelt applies the last rule that matches.
///
/// `com.apple.coreservices.launchservicesd` is deliberately absent: without it
/// AppKit aborts LibreOffice at startup. See the module comment.
const DENIED_MACH_SERVICES: &[&str] = &[
    // LaunchServices' brokers for opening files and URLs, and for changing
    // which app handles what.
    r#"(global-name "com.apple.lsd.open")"#,
    r#"(global-name "com.apple.lsd.openurl")"#,
    r#"(global-name "com.apple.lsd.modifydb")"#,
    // Apple Events, which drive other apps, Terminal included.
    r#"(global-name-prefix "com.apple.coreservices.appleevents")"#,
    // The pasteboard, local and Universal Clipboard.
    r#"(global-name "com.apple.pasteboard.1")"#,
    r#"(global-name "com.apple.coreservices.uauseractivitypasteboardclient.xpc")"#,
    // The keychain, the security agents, and authorization.
    r#"(global-name "com.apple.SecurityServer")"#,
    r#"(global-name-prefix "com.apple.securityd")"#,
    r#"(global-name-prefix "com.apple.security.")"#,
    r#"(global-name "com.apple.authd")"#,
];

/// The confined converter invocation: `sandbox-exec` carrying the profile for
/// this conversion, with the converter resolved to the path the kernel will
/// see. The caller adds the LibreOffice arguments.
///
/// Executing the resolved path rather than the name it was found under is part
/// of the confinement, not tidiness: a launcher on `PATH` is usually a symlink
/// into a directory the profile denies, the profile is written in terms of
/// what paths resolve to, and only the resolved bundle may run.
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
    let bundle = bundle_root(converter);

    let denied = DENIED_READS
        .iter()
        .map(|path| sbpl::subpath_str(path))
        .chain(std::iter::once(DENIED_USER_CACHE.to_owned()))
        .collect::<Vec<_>>()
        .join("\n  ");
    let denied_services = DENIED_MACH_SERVICES.join("\n  ");
    // A bundle under a denied root is reached by path, and LibreOffice stats
    // its way up from the binary; metadata on the ancestors keeps that working
    // without opening the denied trees for reading.
    let bundle_metadata = bundle
        .ancestors()
        .skip(1)
        .map(sbpl::literal)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?
        .join("\n  ");
    let bundle = sbpl::subpath(&bundle).map_err(|error| error.to_string())?;
    let workdir = sbpl::subpath(&workdir).map_err(|error| error.to_string())?;

    Ok(format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec\n  {bundle})\n\
         (allow process-fork)\n\
         (allow signal (target self))\n\
         (allow sysctl-read)\n\
         (allow ipc-posix-shm)\n\
         (allow mach-lookup)\n\
         (deny mach-lookup\n  {denied_services})\n\
         (allow network-bind (local unix))\n\
         (allow network-outbound (local unix) (remote unix))\n\
         (allow file-read*)\n\
         (deny file-read*\n  {denied})\n\
         (allow file-read-metadata\n  {bundle_metadata})\n\
         (allow file-read*\n  {bundle}\n  {workdir})\n\
         (allow file-write*\n  {workdir}\n  \
         (literal \"/private/tmp\")\n  \
         (regex #\"^/private/tmp/OSL_PIPE_[^/]*$\")\n  \
         (literal \"/dev/null\"))\n"
    ))
}

/// The application bundle a binary belongs to: its nearest `.app` ancestor.
pub(crate) fn app_bundle(binary: &Path) -> Option<&Path> {
    binary
        .ancestors()
        .find(|ancestor| ancestor.extension().is_some_and(|ext| ext == "app"))
}

/// The application bundle a converter binary belongs to, or its own directory
/// when the binary is not bundled. Allowing the bundle rather than the binary
/// is required: `soffice` is a launcher that loads the rest of the install.
fn bundle_root(converter: &Path) -> PathBuf {
    app_bundle(converter)
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

    /// A stand-in converter: an `soffice` inside a `LibreOffice.app` bundle
    /// under `install`, resolved the way `confined_command` resolves it.
    fn fake_converter(install: &Path) -> PathBuf {
        let bundle = install.join("LibreOffice.app/Contents/MacOS");
        std::fs::create_dir_all(&bundle).expect("bundle");
        let soffice = bundle.join("soffice");
        std::fs::write(&soffice, b"#!/bin/sh\n").expect("soffice");
        std::fs::canonicalize(&soffice).expect("canonical converter")
    }

    /// The properties the profile exists for: no IP network, no writes outside
    /// the throwaway workdir, and the user's files unreadable.
    #[test]
    fn profile_confines_reads_writes_and_network() {
        let workdir = tempfile::tempdir().expect("workdir");
        let install = tempfile::tempdir().expect("install");
        let converter = fake_converter(install.path());

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

    /// Only the converter's own bundle may run, and the Mach services that
    /// lead out of the sandbox are denied after the broad allowance. Seatbelt
    /// applies the last rule that matches, so the order is the contract.
    #[test]
    fn profile_runs_only_the_bundle_and_denies_services_that_lead_out() {
        let workdir = tempfile::tempdir().expect("workdir");
        let install = tempfile::tempdir().expect("install");
        let converter = fake_converter(install.path());
        let bundle = sbpl::subpath(&bundle_root(&converter)).expect("bundle clause");

        let profile = profile(&converter, workdir.path()).expect("profile");

        assert!(
            profile.contains(&format!("(allow process-exec\n  {bundle})\n")),
            "{profile}"
        );
        assert!(!profile.contains("(allow process-exec)"), "{profile}");
        let allowed = profile
            .find("(allow mach-lookup)")
            .expect("profile allows mach-lookup");
        let denied = profile
            .find("(deny mach-lookup")
            .expect("profile denies some mach-lookup");
        assert!(allowed < denied, "{profile}");
        for service in [
            "com.apple.lsd.open",
            "com.apple.lsd.openurl",
            "com.apple.lsd.modifydb",
            "com.apple.coreservices.appleevents",
            "com.apple.pasteboard.1",
            "com.apple.SecurityServer",
            "com.apple.securityd",
            "com.apple.security.",
            "com.apple.authd",
        ] {
            assert!(
                profile[denied..].contains(&format!("\"{service}\"")),
                "{service} is not denied: {profile}"
            );
        }
        // AppKit aborts LibreOffice at startup without launchservicesd, and
        // LaunchServices refuses to open files for this sandbox only while
        // `lsopen` stays denied.
        assert!(!profile.contains("launchservicesd"), "{profile}");
        assert!(!profile.contains("lsopen"), "{profile}");
    }

    /// A crafted document that takes over the converter cannot start `open`
    /// or `osascript` to run a file it wrote outside the sandbox: the kernel
    /// refuses to start either one. The `open` target is System Events, which
    /// has no windows, so a regression here launches nothing visible.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandboxed_converter_cannot_start_open_or_osascript() {
        let workdir = tempfile::tempdir().expect("workdir");
        let install = tempfile::tempdir().expect("install");
        let converter = fake_converter(install.path());
        let profile = profile(&converter, workdir.path()).expect("profile");

        for program in [
            &["/usr/bin/open", "-g", "-j", "-b", "com.apple.systemevents"][..],
            &["/usr/bin/osascript", "-e", "return 1"][..],
        ] {
            let output = std::process::Command::new(sbpl::SANDBOX_EXEC)
                .arg("-p")
                .arg(&profile)
                .arg("--")
                .args(program)
                .current_dir(workdir.path())
                .env_clear()
                .env("HOME", workdir.path())
                .env("TMPDIR", workdir.path())
                .stdin(std::process::Stdio::null())
                .output()
                .expect("run sandbox-exec");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success(),
                "{program:?} ran under the converter profile"
            );
            assert!(
                stderr.contains("execvp()") && stderr.contains("Operation not permitted"),
                "{program:?} started under the converter profile: {stderr}"
            );
        }
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
